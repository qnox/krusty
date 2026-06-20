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
}

impl KspHost {
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
            max_rounds: 100,
        }
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
            if rounds >= self.max_rounds {
                break; // backstop against a non-terminating processor chain
            }

            for f in &fresh {
                seen_files.push(f.name.clone());
                symbols.extend(f.declares.iter().cloned()); // re-resolve: generated symbols join the view
            }
            generated.extend(fresh);
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
pub fn symbols_from_ir(ir: &IrFile, ctx: &super::PluginContext) -> Vec<KsClass> {
    ir.classes
        .iter()
        .enumerate()
        .map(|(i, c)| KsClass {
            fq_name: c.fq_name.clone(),
            annotations: ctx
                .class_annotations
                .get(&(i as u32))
                .cloned()
                .unwrap_or_default(),
            properties: c
                .fields
                .iter()
                .map(|(name, ty)| KsProp {
                    name: name.clone(),
                    ty: KsType {
                        fq_name: match ty {
                            crate::ir::IrType::Class { fq_name, .. } => fq_name.clone(),
                            _ => "kotlin/Any".to_string(),
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

    fn annotated_foo() -> (IrFile, PluginContext) {
        let mut ir = IrFile::default();
        let id = ir.add_class(synthetic_class("demo/Foo"));
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(id, vec![GEN_BUILDER.to_string()]);
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
