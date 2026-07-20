//! Codegen host — the in-process host for KSP (and, by the same shape, APT). See `docs/PLUGIN_API.md`.
//!
//! KSP processors are **codegen-only**: they READ a resolved symbol view (`Resolver`) and EMIT new
//! source files (`CodeGenerator.createNewFile`); they never mutate existing declarations. That
//! contract is shim-able across a process boundary — in production a JVM sidecar loads the real
//! processor JAR and a shim JAR implements KSP's `Resolver`/`KSClassDeclaration` interfaces, each
//! method an IPC call into krusty's resolver. This module models that boundary in Rust to prove the
//! front-stage **fixpoint** pipeline:
//!
//!   resolve → run processors over the symbol view → collect generated files
//!           → re-resolve (generated files add symbols) → repeat until a round emits nothing new.
//!
//! The [`Resolver`] is deliberately a read-only, span-carrying semantic view — the SAME shape a
//! future LSP queries for completion/hover/go-to-def (see the doc's dual-use section).

use crate::ir::IrFile;

/// KSP is released **per Kotlin compiler version** — artifacts are coordinated `<kotlin>-<ksp>`
/// (e.g. `2.0.21-1.0.28`) and KSP's behavior depends on the compiler it embeds. So the sidecar's
/// toolchain is *determined by* the kotlinc version krusty targets, not chosen independently.
///
/// The pair is RESOLVED BY THE BUILD (the dependency manifest pins both), then handed to the host —
/// krusty bakes no kotlin→ksp table (same rule as the rest of the compiler). The host's only job is
/// to guarantee the spawned sidecar uses exactly this pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspToolchain {
    pub kotlin_version: String,
    pub ksp_version: String,
}

impl KspToolchain {
    pub fn new(kotlin_version: impl Into<String>, ksp_version: impl Into<String>) -> Self {
        Self {
            kotlin_version: kotlin_version.into(),
            ksp_version: ksp_version.into(),
        }
    }

    /// Sanity check the build-resolved pair is internally consistent: a KSP coordinate is prefixed by
    /// the Kotlin version it targets (`2.0.21-1.0.28` ← Kotlin `2.0.21`). Catches a misconfigured
    /// manifest before a version-mismatched sidecar produces subtly wrong symbols.
    pub fn is_consistent(&self) -> bool {
        self.ksp_version
            .strip_prefix(&self.kotlin_version)
            .is_some_and(|rest| rest.starts_with('-'))
    }
}

/// A source location, so the symbol view can answer go-to-def for an LSP. (`u32` line/col keep it
/// `Copy` and index-based, per krusty conventions.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: u32,
    pub line: u32,
}

/// A resolved type reference as a processor / LSP sees it (Kotlin FqName). Generics would carry
/// `type_args` here — the real blocker for a production KSP host, since `Resolver` exposes
/// parameterized types everywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KsType {
    pub fq_name: String,
}

/// A resolved property (KSP `KSPropertyDeclaration`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KsProp {
    pub name: String,
    pub ty: KsType,
}

/// A resolved class declaration (KSP `KSClassDeclaration`) — the unit a processor reads and the unit
/// a re-resolved generated file contributes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KsClass {
    pub fq_name: String,
    pub annotations: Vec<String>,
    pub properties: Vec<KsProp>,
    pub span: SourceSpan,
}

/// The read-only resolved symbol view (KSP `Resolver`; an LSP's semantic query surface). Backed by a
/// slice of `KsClass` — in production, by an adapter over krusty's `SymbolTable`/`TypeInfo` (and, via
/// the shim, exposed to the JVM processor as the real `Resolver` interface).
pub struct Resolver<'a> {
    symbols: &'a [KsClass],
}

impl<'a> Resolver<'a> {
    pub fn new(symbols: &'a [KsClass]) -> Self {
        Self { symbols }
    }

    /// KSP `getSymbolsWithAnnotation`.
    pub fn get_symbols_with_annotation(&self, fq: &str) -> Vec<&'a KsClass> {
        self.symbols
            .iter()
            .filter(|c| c.annotations.iter().any(|a| a == fq))
            .collect()
    }

    /// KSP `getAllFiles`/class enumeration; also the LSP "workspace symbols" query.
    pub fn get_all_classes(&self) -> &'a [KsClass] {
        self.symbols
    }

    /// LSP go-to-def: resolve an FqName to its declaration span.
    pub fn span_of(&self, fq: &str) -> Option<SourceSpan> {
        self.symbols
            .iter()
            .find(|c| c.fq_name == fq)
            .map(|c| c.span)
    }
}

/// A file emitted by a processor (KSP `CodeGenerator.createNewFile`). `declares` is what the file
/// contributes to the symbol view once re-parsed + re-resolved — modeling the fixpoint feedback
/// without a real parse round-trip in this PoC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    pub name: String,
    pub content: String,
    pub declares: Vec<KsClass>,
}

/// Captures generated files. CODEGEN ONLY — it appends files and is the *only* output channel; a
/// processor has no handle to mutate existing IR. (Enforced structurally: `process` receives `&self`
/// `Resolver` + `&mut CodeGenerator`, never the `IrFile`.)
#[derive(Default)]
pub struct CodeGenerator {
    pub files: Vec<GeneratedFile>,
}

impl CodeGenerator {
    pub fn create_new_file(&mut self, name: &str, content: &str, declares: Vec<KsClass>) {
        self.files.push(GeneratedFile {
            name: name.to_string(),
            content: content.to_string(),
            declares,
        });
    }
}

/// Stands for the JVM processor JAR across the shim (KSP `SymbolProcessorProvider` →
/// `SymbolProcessor.process(resolver)`). The real implementation runs unmodified on the sidecar.
pub trait SymbolProcessor {
    fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator);
}

/// Result of a host run: every file generated, and how many rounds it took to reach the fixpoint.
#[derive(Debug, Default)]
pub struct KspResult {
    pub generated: Vec<GeneratedFile>,
    pub rounds: u32,
}

/// Drives processors to a fixpoint. Each round: build a `Resolver` over the current symbols, run
/// every processor, collect files not seen before; the new files' `declares` extend the symbol view
/// for the next round. Stops when a round emits nothing new (or hits the round backstop).
pub struct KspHost {
    processors: Vec<Box<dyn SymbolProcessor>>,
    max_rounds: u32,
    /// The build-resolved Kotlin/KSP toolchain the sidecar must run. `None` in the pure in-process
    /// PoC (no real sidecar); `Some` once wired to spawn the version-matched JVM.
    toolchain: Option<KspToolchain>,
}

impl KspHost {
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
            max_rounds: 100,
            toolchain: None,
        }
    }

    /// Pin the host to the build-resolved toolchain (tied to krusty's targeted kotlinc version).
    pub fn for_toolchain(toolchain: KspToolchain) -> Self {
        Self {
            toolchain: Some(toolchain),
            ..Self::new()
        }
    }

    pub fn toolchain(&self) -> Option<&KspToolchain> {
        self.toolchain.as_ref()
    }

    /// Lower the round backstop — for tests, and for a CI policy that caps a misbehaving processor
    /// chain rather than letting it run away.
    pub fn with_max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    pub fn register(&mut self, p: Box<dyn SymbolProcessor>) {
        self.processors.push(p);
    }

    /// Extract the initial symbol view from the checked program. In production this reads
    /// `SymbolTable`/`TypeInfo`; here it lifts the `KsClass` list a caller built from the IR + the
    /// annotation table. The host NEVER takes `&mut IrFile` — codegen cannot touch existing decls.
    pub fn run(&mut self, initial_symbols: Vec<KsClass>) -> KspResult {
        let mut symbols = initial_symbols;
        let mut seen_files: Vec<String> = Vec::new();
        let mut generated: Vec<GeneratedFile> = Vec::new();
        let mut rounds = 0;

        loop {
            rounds += 1;
            let mut gen = CodeGenerator::default();
            {
                let resolver = Resolver::new(&symbols);
                for p in &mut self.processors {
                    p.process(&resolver, &mut gen);
                }
            }

            // Keep only files not produced in a prior round (idempotent processors re-emit).
            let fresh: Vec<GeneratedFile> = gen
                .files
                .into_iter()
                .filter(|f| !seen_files.contains(&f.name))
                .collect();

            if fresh.is_empty() {
                break; // fixpoint reached
            }

            // Commit this round's output, then enforce the backstop — so a capped run still keeps
            // the work it did rather than discarding the final round.
            for f in &fresh {
                seen_files.push(f.name.clone());
                symbols.extend(f.declares.iter().cloned()); // re-resolve: generated symbols join the view
            }
            generated.extend(fresh);

            if rounds >= self.max_rounds {
                break; // backstop against a non-terminating processor chain
            }
        }

        KspResult { generated, rounds }
    }
}

impl Default for KspHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Lift `IrClass`es + an annotation side table into the `KsClass` symbol view a processor reads.
/// (Production: an adapter over the resolved `SymbolTable`/`TypeInfo`, exposed via the shim.)
pub fn symbols_from_ir(ir: &IrFile, ctx: &super::PluginContext<'_>) -> Vec<KsClass> {
    ir.classes
        .iter()
        .enumerate()
        .map(|(i, c)| KsClass {
            fq_name: c.fq_name(),
            annotations: ctx
                .class_annotations
                .get(&(i as u32))
                .map(|annotations| annotations.as_slice().to_vec())
                .unwrap_or_default(),
            properties: c
                .fields
                .iter()
                .map(|f| KsProp {
                    name: f.name.clone(),
                    ty: KsType {
                        fq_name: match f.ty.non_null().obj_internal() {
                            Some(fq) => fq.to_string(),
                            None => "kotlin/Any".to_string(),
                        },
                    },
                })
                .collect(),
            span: SourceSpan {
                file: 0,
                line: i as u32,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{synthetic_class, PluginContext};

    const GEN_BUILDER: &str = "demo/GenerateBuilder";
    const GEN_VALIDATOR: &str = "demo/GenerateValidator";

    /// `@GenerateBuilder Foo` → emits `FooBuilder`, itself annotated `@GenerateValidator`.
    struct BuilderProcessor;
    impl SymbolProcessor for BuilderProcessor {
        fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator) {
            for c in resolver.get_symbols_with_annotation(GEN_BUILDER) {
                let builder_fq = format!("{}Builder", c.fq_name);
                gen.create_new_file(
                    &builder_fq,
                    &format!("class {builder_fq} {{ /* generated */ }}"),
                    vec![KsClass {
                        fq_name: builder_fq.clone(),
                        annotations: vec![GEN_VALIDATOR.to_string()],
                        properties: Vec::new(),
                        span: SourceSpan::default(),
                    }],
                );
            }
        }
    }

    /// `@GenerateValidator X` → emits `XValidator` (un-annotated → chain terminates).
    struct ValidatorProcessor;
    impl SymbolProcessor for ValidatorProcessor {
        fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator) {
            for c in resolver.get_symbols_with_annotation(GEN_VALIDATOR) {
                let v_fq = format!("{}Validator", c.fq_name);
                gen.create_new_file(
                    &v_fq,
                    &format!("class {v_fq} {{ /* generated */ }}"),
                    vec![KsClass {
                        fq_name: v_fq.clone(),
                        annotations: Vec::new(),
                        properties: Vec::new(),
                        span: SourceSpan::default(),
                    }],
                );
            }
        }
    }

    fn annotated_foo() -> (IrFile, PluginContext<'static>) {
        let mut ir = IrFile::default();
        let id = ir.add_class(synthetic_class("demo/Foo"));
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(id, vec![GEN_BUILDER.to_string()].into());
        (ir, ctx)
    }

    #[test]
    fn resolver_finds_annotated_symbols_and_spans() {
        let (ir, ctx) = annotated_foo();
        let symbols = symbols_from_ir(&ir, &ctx);
        let resolver = Resolver::new(&symbols);
        let hits = resolver.get_symbols_with_annotation(GEN_BUILDER);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].fq_name, "demo/Foo");
        // LSP-style go-to-def works over the same view.
        assert!(resolver.span_of("demo/Foo").is_some());
        assert!(resolver.span_of("demo/Missing").is_none());
    }

    #[test]
    fn fixpoint_runs_chained_processors_to_termination() {
        let (ir, ctx) = annotated_foo();
        let mut host = KspHost::new();
        host.register(Box::new(BuilderProcessor));
        host.register(Box::new(ValidatorProcessor));

        let result = host.run(symbols_from_ir(&ir, &ctx));

        let names: Vec<&str> = result.generated.iter().map(|f| f.name.as_str()).collect();
        // Round 1 generates the builder; round 2 sees its @GenerateValidator and generates the
        // validator; round 3 finds nothing new → terminate.
        assert_eq!(names, vec!["demo/FooBuilder", "demo/FooBuilderValidator"]);
        assert_eq!(result.rounds, 3, "two productive rounds + one empty round");
    }

    #[test]
    fn codegen_is_output_only_input_ir_unchanged() {
        let (ir, ctx) = annotated_foo();
        let classes_before = ir.classes.len();
        let mut host = KspHost::new();
        host.register(Box::new(BuilderProcessor));
        host.register(Box::new(ValidatorProcessor));

        // The host consumes a symbol snapshot, never `&mut IrFile`: existing decls cannot be mutated.
        let _ = host.run(symbols_from_ir(&ir, &ctx));
        assert_eq!(
            ir.classes.len(),
            classes_before,
            "codegen never mutates input IR"
        );
    }

    #[test]
    fn generates_one_builder_per_annotated_class() {
        let mut ir = IrFile::default();
        let mut ctx = PluginContext::default();
        for name in ["demo/A", "demo/B", "demo/C"] {
            let id = ir.add_class(synthetic_class(name));
            ctx.class_annotations
                .insert(id, vec![GEN_BUILDER.to_string()].into());
        }
        let mut host = KspHost::new();
        host.register(Box::new(BuilderProcessor));
        let result = host.run(symbols_from_ir(&ir, &ctx));
        let mut names: Vec<&str> = result.generated.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["demo/ABuilder", "demo/BBuilder", "demo/CBuilder"]
        );
    }

    /// A processor whose generated class re-triggers the SAME annotation would never converge — the
    /// `max_rounds` backstop must stop it deterministically rather than hang.
    struct RunawayProcessor;
    impl SymbolProcessor for RunawayProcessor {
        fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator) {
            for c in resolver.get_symbols_with_annotation(GEN_BUILDER) {
                let next = format!("{}X", c.fq_name);
                gen.create_new_file(
                    &next,
                    "/* generated */",
                    vec![KsClass {
                        fq_name: next.clone(),
                        annotations: vec![GEN_BUILDER.to_string()], // re-triggers itself forever
                        properties: Vec::new(),
                        span: SourceSpan::default(),
                    }],
                );
            }
        }
    }

    #[test]
    fn runaway_chain_stops_at_max_rounds() {
        let (ir, ctx) = annotated_foo();
        let mut host = KspHost::new().with_max_rounds(5);
        host.register(Box::new(RunawayProcessor));
        let result = host.run(symbols_from_ir(&ir, &ctx));
        assert_eq!(result.rounds, 5, "backstop caps a non-converging chain");
        // Each productive round emits exactly one new file; the final (capped) round is productive too.
        assert_eq!(result.generated.len(), 5);
    }

    #[test]
    fn toolchain_is_pinned_and_consistency_checked() {
        // KSP version is tied to the kotlinc version; the build resolves the pair and pins the host.
        let tc = KspToolchain::new("2.0.21", "2.0.21-1.0.28");
        assert!(
            tc.is_consistent(),
            "ksp coordinate is prefixed by its kotlin version"
        );
        let host = KspHost::for_toolchain(tc.clone());
        assert_eq!(host.toolchain(), Some(&tc));

        // A mismatched manifest (ksp built for a different kotlin) is caught.
        let bad = KspToolchain::new("2.4.0", "2.0.21-1.0.28");
        assert!(!bad.is_consistent());
    }

    #[test]
    fn is_consistent_rejects_partial_version_match() {
        // A prefix that isn't a whole version component must NOT pass: kotlin "2.0.2" is a string
        // prefix of "2.0.21-..." but a different version — the '-' boundary check rejects it.
        assert!(!KspToolchain::new("2.0.2", "2.0.21-1.0.28").is_consistent());
        // Missing the '-<ksp>' suffix entirely is rejected.
        assert!(!KspToolchain::new("2.0.21", "2.0.21").is_consistent());
        // Empty kotlin version can't anchor a coordinate.
        assert!(!KspToolchain::new("", "2.0.21-1.0.28").is_consistent());
        assert!(!KspToolchain::new("", "").is_consistent());
    }

    #[test]
    fn terminates_with_no_annotated_symbols() {
        let mut ir = IrFile::default();
        ir.add_class(synthetic_class("demo/Plain"));
        let mut host = KspHost::new();
        host.register(Box::new(BuilderProcessor));
        let result = host.run(symbols_from_ir(&ir, &PluginContext::default()));
        assert!(result.generated.is_empty());
        assert_eq!(result.rounds, 1, "single empty round → immediate fixpoint");
    }
}
