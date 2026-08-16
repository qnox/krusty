//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::ast::{Decl, File};
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::frontend::{CheckedFile, FrontendSymbols};
use crate::jvm::names::{file_class_name, type_descriptor};
use crate::types::{type_name, Ty};

/// Why [`run_backend_passes`] declined a file: the named pass met a shape it can't lower yet, so the
/// caller must skip (or diagnose) the file rather than miscompile it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `lower_value_classes` — a `@JvmInline value class` shape not yet supported.
    ValueClasses,
    /// `lower_suspend` — a `suspend fun` shape not yet supported.
    Suspend,
    /// `derive_bridges` — an override whose bridge cannot be modeled (a bounded type-param erasure, or a
    /// `suspend` override the coroutine pass would rewrite out from under the bridge).
    Bridges,
}

/// THE post-lowering, pre-emit JVM pass pipeline — the single definition every consumer (the real
/// backend, `tests/common`, the conformance harness, `bytediff`, `survey`) must call, so a newly
/// added pass lands in all of them by construction. Hand-replicating this sequence has twice
/// produced false-green test runs (a pass added here but missed in a replica → IllegalAccessError
/// miscompiles the gate never saw); a unit test below bans direct calls to the individual passes.
///
/// Runs, in order:
/// 1. `plugins::run_enabled` — compiler-extension plugins (kotlinx.serialization) synthesize
///    declarations from the file's annotations; no-op without a trigger annotation.
/// 2. `lower_companion_properties` — realize supported companion backing fields as JVM outer statics.
///    Common IR keeps the ordinary declaration and semantic initializer for other targets.
/// 3. `elide_default_property_stores` — omit declaration stores already supplied by JVM field
///    initialization. Common IR retains them for targets without zero-initialized fields.
/// 4. `derive_bridges` — synthesize the `ACC_BRIDGE` methods an override needs to be reachable through
///    a supertype's erased descriptor. A bridge is a JVM realization of an override, not a Kotlin
///    declaration, so lowering records only the declarations and this pass derives the bridges.
/// 5. `apply_collection_bridge_barriers` — attach JVM collection bridge semantics.
/// 6. `lower_value_classes` — realize `@JvmInline value class`es as their unboxed underlying type
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected).
/// 7. `lower_suspend` — realize `suspend fun`s as their continuation-passing-style ABI.
/// 8. `mark_must_inline_lambdas` — drop the dead standalone impl of a must-inline call's
///    (`require`/`check`) message lambda; it is spliced at the call site.
/// 9. `reparent_lambda_impls` — a lambda impl method must be a member of the CLASS whose code emits
///    its `invokedynamic` (the impl is PRIVATE, kotlinc's placement, so a cross-class handle would
///    be an IllegalAccessError). Lowering attaches impls per `cur_class`, which misses code that
///    ends up in a class only later: enum-entry constructor arguments and suspend-lambda state
///    machines. Runs after all IR→IR transforms, before emit.
///
/// Per-site concerns (timing counters, bail-reason strings, diagnostics) stay at the call sites.
pub fn run_backend_passes(
    ir: &mut crate::ir::IrFile,
    file: &File,
    facade: &str,
    module_name: &str,
    syms: &FrontendSymbols,
) -> Result<(), SkipReason> {
    let mut discard = crate::jvm::suspend::ContinuationMetadataMap::default();
    run_backend_passes_with_metadata(ir, file, facade, module_name, syms, &mut discard)
}

/// Run the JVM pass pipeline and retain continuation metadata for class emission.
pub fn run_backend_passes_with_metadata(
    ir: &mut crate::ir::IrFile,
    file: &File,
    facade: &str,
    module_name: &str,
    syms: &FrontendSymbols,
    continuation_metadata: &mut crate::jvm::suspend::ContinuationMetadataMap,
) -> Result<(), SkipReason> {
    let resolve_class_name = |name: &str| syms.class_names.get(name);
    crate::plugins::run_enabled(
        ir,
        file,
        module_name,
        &resolve_class_name,
        jvm_plugin_type_descriptor,
    );
    crate::jvm::annotation_constructions::lower_annotation_constructions(ir, facade);
    // Companion backing-field hoisting is a JVM storage choice. Common IR retains the ordinary
    // property declaration and semantic initializer; this pass selects the outer-static realization.
    crate::jvm::companion::lower_companion_properties(ir);
    // The JVM supplies default field values before any constructor runs. Elide only source
    // declaration stores recorded by exact ExprId; common IR and other targets keep them.
    crate::jvm::property_storage::elide_default_property_stores(ir);
    // Bridges are a JVM realization of an override, derived here from the IR's own declarations and the
    // checker's supertype view. Runs BEFORE the barrier pass (which annotates existing bridges) and
    // before the value-class pass (which retargets them once mangled names are known).
    crate::jvm::bridges::derive_bridges(ir, syms)?;
    apply_collection_bridge_barriers(ir, syms);
    let vc_module = crate::module_symbols::ModuleSymbols::new(syms);
    let vc_resolver = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
        &*syms.libraries,
        &vc_module,
        &[],
    );
    // Same-module SOURCE value classes (internal name → sole-field underlying) for the value-class pass's
    // erasure/mangle map — a value class declared in ANOTHER file of this module. Read from the frontend
    // symbols directly, NOT surfaced through the resolver's library view (which would change the checker's
    // construction/member resolution for source value classes).
    let module_value_classes: std::collections::HashMap<_, _> = syms
        .classes
        .values()
        .filter_map(|c| c.value_field.as_ref().map(|(_, t)| (c.internal_name(), *t)))
        .collect();
    crate::jvm::value_classes::apply_override_final_drop(ir, &vc_resolver);
    if !crate::jvm::value_classes::lower_value_classes(ir, &vc_resolver, &module_value_classes) {
        return Err(SkipReason::ValueClasses);
    }
    if !crate::jvm::suspend::lower_suspend(ir, facade, continuation_metadata) {
        return Err(SkipReason::Suspend);
    }
    crate::jvm::ir_emit::mark_must_inline_lambdas(ir);
    crate::jvm::ir_emit::reparent_lambda_impls(ir);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BridgeBarrierOutcome {
    False,
    NotFound,
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BridgeBarrier {
    pub parameter: usize,
    pub outcome: BridgeBarrierOutcome,
}

#[derive(Clone, Copy)]
enum CollectionOwner {
    Collection,
    MutableCollection,
    List,
    Map,
}

impl CollectionOwner {
    fn matches(self, owner: crate::types::TypeName) -> bool {
        let names: &[&str] = match self {
            CollectionOwner::Collection => &[
                "java/util/Collection",
                "java/util/List",
                "java/util/Set",
                "kotlin/collections/Collection",
                "kotlin/collections/MutableCollection",
                "kotlin/collections/List",
                "kotlin/collections/MutableList",
                "kotlin/collections/Set",
                "kotlin/collections/MutableSet",
            ],
            CollectionOwner::MutableCollection => &[
                "java/util/Collection",
                "java/util/List",
                "java/util/Set",
                "kotlin/collections/MutableCollection",
                "kotlin/collections/MutableList",
                "kotlin/collections/MutableSet",
            ],
            CollectionOwner::List => &[
                "java/util/List",
                "kotlin/collections/List",
                "kotlin/collections/MutableList",
            ],
            CollectionOwner::Map => &[
                "java/util/Map",
                "kotlin/collections/Map",
                "kotlin/collections/MutableMap",
            ],
        };
        names.iter().any(|name| owner.matches(name))
    }
}

fn collection_bridge_semantics(
    bridge: &crate::ir::Bridge,
) -> Option<(CollectionOwner, BridgeBarrier)> {
    let (owner, outcome) = match bridge.name.as_str() {
        "contains"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (CollectionOwner::Collection, BridgeBarrierOutcome::False)
        }
        "remove"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (
                CollectionOwner::MutableCollection,
                BridgeBarrierOutcome::False,
            )
        }
        "indexOf" | "lastIndexOf"
            if bridge.erased_ret == crate::types::Ty::Int
                && bridge.concrete_ret == crate::types::Ty::Int =>
        {
            (CollectionOwner::List, BridgeBarrierOutcome::NotFound)
        }
        "containsKey"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (CollectionOwner::Map, BridgeBarrierOutcome::False)
        }
        "get" if bridge.erased_ret.is_reference() && bridge.concrete_ret.is_reference() => {
            (CollectionOwner::Map, BridgeBarrierOutcome::Null)
        }
        _ => return None,
    };
    let parameter = 0;
    (bridge.erased_params.len() == 1
        && bridge.concrete_params.len() == 1
        && bridge.erased_params[parameter].is_erased_top()
        && bridge.concrete_params[parameter].is_reference()
        && !bridge.concrete_params[parameter].is_erased_top())
    .then_some((parameter, outcome))
    .map(|(parameter, outcome)| (owner, BridgeBarrier { parameter, outcome }))
}

pub(crate) fn bridge_barrier(bridge: &crate::ir::Bridge) -> Option<BridgeBarrier> {
    bridge
        .type_safe_barrier
        .then(|| collection_bridge_semantics(bridge))
        .flatten()
        .map(|(_, barrier)| barrier)
}

fn apply_collection_bridge_barriers(ir: &mut crate::ir::IrFile, syms: &FrontendSymbols) {
    for class in &mut ir.classes {
        let owners = syms
            .applied_hierarchy(crate::types::Ty::obj_name(class.fq_name))
            .into_iter()
            .map(|(owner, _, _)| owner)
            .collect::<Vec<_>>();
        for bridge in &mut class.bridges {
            bridge.type_safe_barrier =
                collection_bridge_semantics(bridge).is_some_and(|(required, _)| {
                    owners.iter().copied().any(|owner| required.matches(owner))
                });
        }
    }
}

fn jvm_plugin_type_descriptor(ty: Ty) -> Option<String> {
    Some(type_descriptor(ty))
}

/// The JVM backend holds the shared classpath (`Rc`, same instance as `JvmLibraries`) so the emitter
/// can read inline-function bodies for the bytecode inliner.
pub struct JvmBackend {
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
    /// Class-file major version to emit (`-jvm-target`), or `None` for krusty's default (v52).
    class_major: Option<u16>,
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
    lambdas: crate::jvm::ir_emit::LambdaMode,
    /// Whether to emit the `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions`
    /// clears this).
    param_assertions: bool,
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<crate::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend {
            cp,
            class_major: None,
            jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
            lambdas: crate::jvm::ir_emit::LambdaMode::default(),
            param_assertions: true,
        }
    }

    /// `-Xno-param-assertions` passes `false`: emit no `Intrinsics.checkNotNullParameter` guards.
    pub fn with_param_assertions(mut self, enabled: bool) -> JvmBackend {
        self.param_assertions = enabled;
        self
    }

    /// `-jvm-default`: which JVM shape an interface's members with bodies are compiled into.
    pub fn with_jvm_default(mut self, mode: crate::jvm::ir_emit::JvmDefaultMode) -> JvmBackend {
        self.jvm_default = mode;
        self
    }

    /// `-Xlambdas` / `-Xsam-conversions`: whether a lambda becomes an `invokedynamic` call site or
    /// its own class.
    pub fn with_lambdas(mut self, mode: crate::jvm::ir_emit::LambdaMode) -> JvmBackend {
        self.lambdas = mode;
        self
    }

    /// Set the class-file version subsequent emits target (from the CLI's `-jvm-target`).
    pub fn with_class_major(mut self, major: Option<u16>) -> JvmBackend {
        self.class_major = major;
        self
    }
}

/// The per-file emit configuration krusty SHIPS with — ONE definition, so an in-process caller (the
/// test harness, an embedder) emits exactly what `krusty -d …` does. It carries the class version
/// (`-jvm-target`), the `SourceFile` name (the origin `.kt`; kotlinc uses the simple name,
/// reconstructed from the stem — directories are already stripped), the `-module-name`, and the
/// per-class `@Metadata` switch. Threaded explicitly into emission so every class (incl. synthetics)
/// inherits it. `EmitOptions::default()` is NOT the pre-class-metadata shape — it emits per-class
/// `@Metadata` too; what it lacks is the `SourceFile`, the inner-class resolver and the `-jvm-target`
/// class version, which is why a caller claiming to emit shipping bytes must start here.
/// `KRUSTY_NO_CLASS_METADATA` (read below) is how a shipping caller gets facade-only output back; it is
/// consulted ONLY here, so a caller holding some other `EmitOptions` opts out by setting
/// `emit_class_metadata: false` itself.
pub fn shipping_emit_options(
    stem: &str,
    module_name: &str,
    class_major: Option<u16>,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::ir_emit::EmitOptions {
    // Module/conformance inputs may retain a source-relative prefix (`helpers/Foo`) even though the
    // CLI has already reduced its input to `Foo`. `SourceFile` is a simple filename on every JVM
    // class, so normalize at this shared boundary instead of making each non-CLI caller grow its own
    // path branch. Accept both separators because Kotlin testdata names are logical source paths and
    // are not guaranteed to use the host platform's separator.
    let source_stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    crate::jvm::ir_emit::EmitOptions {
        class_major,
        source_file: Some(format!("{source_stem}.kt")),
        // kotlinc records `classModuleName` in @Metadata unless the module is the default `main`.
        module_name: (module_name != "main").then(|| module_name.to_string()),
        // Per-INVOCATION strategy; the CLI overrides it with `EmitOptions::with_lambdas`.
        lambdas: crate::jvm::ir_emit::LambdaMode::default(),
        // Compute + emit each class's own `@Metadata`. Without it a krusty-compiled CLASS is
        // unreadable BY KRUSTY: the facade metadata describes top-level declarations only, so a
        // second compilation sees no constructor/member parameter names (named arguments) and no
        // `operator` marks (destructuring). A shape `build_class_metadata` has not verified against
        // kotlinc declines individually and emits nothing, so this cannot write an unverified
        // payload. `KRUSTY_NO_CLASS_METADATA` restores the facade-only output for bisecting.
        emit_class_metadata: std::env::var_os("KRUSTY_NO_CLASS_METADATA").is_none(),
        jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
        // `-Xno-param-assertions` is applied by the caller (`with_param_assertions`); the shipping
        // default emits the guards, as kotlinc does.
        param_assertions: true,
        inner_class_resolver: Some(classpath_inner_class_resolver(cp)),
    }
}

pub fn classpath_inner_class_resolver(
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    std::rc::Rc::new(move |internal: &str| {
        // The class file first — it is authoritative whenever the nested class has one. A mapped
        // builtin whose JVM class is absent (no JDK on the classpath) still declares the same nesting
        // in its `.kotlin_builtins` entry; without this fallback the reference emits no `InnerClasses`
        // attribute at all, so the JDK-less class file diverges from the JDK-present one.
        cp.find(internal)
            .and_then(|class| {
                let entry = class.inner_class_self()?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: entry.outer.clone(),
                    name: entry.name.clone(),
                    access: entry.access,
                })
            })
            .or_else(|| {
                let (outer, name, access) = cp.builtin_nested_class(internal)?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: Some(outer),
                    name: Some(name),
                    access,
                })
            })
    })
}

pub fn prepare_module_symbols(files: &[File], stems: &[String], syms: &mut FrontendSymbols) {
    if files.len() <= 1 {
        return;
    }

    let mut fns: Vec<(u32, u32, Option<String>, String)> = Vec::new();
    let mut unemitted_fns: Vec<(u32, crate::ast::DeclId)> = Vec::new();
    let mut props: Vec<(u32, u32, String, String)> = Vec::new();
    let mut ext_props: Vec<(u32, u32, String)> = Vec::new();
    for (i, (file, stem)) in files.iter().zip(stems).enumerate() {
        let facade = file_class_name(stem, file.package.as_deref());
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    // This is the single owner of the emitted/unemitted decision. The checker
                    // consumes the declaration-keyed outcome recorded below; it must not repeat
                    // this predicate because signature/body support evolves with JVM lowering.
                    // The semantic handoff says whether a callable body exists; this JVM boundary
                    // owns only its representation as a facade static. Common IR lowering consumes
                    // the same answer, so registration cannot promise a body that lowering omits.
                    let emitted = syms.source_fn_has_callable_body(file, i as u32, f);
                    if emitted {
                        fns.push((
                            i as u32,
                            d.0,
                            f.receiver.is_none().then(|| f.name.clone()),
                            facade.clone(),
                        ));
                    } else {
                        // Negative registration is as important as the facade map: it distinguishes
                        // a deliberately splice-only function from a checker-only pipeline where no
                        // JVM registration ran at all.
                        unemitted_fns.push((i as u32, d));
                    }
                }
                Decl::Property(p) if p.receiver.is_none() => {
                    props.push((i as u32, d.0, p.name.clone(), facade.clone()))
                }
                Decl::Property(_) => ext_props.push((i as u32, d.0, facade.clone())),
                _ => {}
            }
        }
    }

    for (file_index, decl_id, name, facade) in fns {
        let facade = type_name(&facade);
        syms.record_fn_facade(file_index, crate::ast::DeclId(decl_id), Some(facade));
        if let Some(name) = name {
            syms.fn_facades.insert(name, facade);
        }
    }
    for (file_index, declaration) in unemitted_fns {
        syms.record_fn_facade(file_index, declaration, None);
    }
    for (file, declaration, name, facade) in props {
        syms.prop_facades_by_decl
            .insert((file, declaration), type_name(&facade));
        if let Some(&(ty, is_var, is_const)) = syms.props.get(&name) {
            syms.prop_facades
                .insert(name, (type_name(&facade), ty, is_var, is_const));
        }
    }
    for (file_index, decl_id, facade) in ext_props {
        syms.ext_prop_facades_by_decl
            .insert((file_index, decl_id), type_name(&facade));
    }
}

/// package → file-facade class names, accumulated across files for the `.kotlin_module` mapping.
#[derive(Default)]
pub struct JvmState {
    module_packages: std::collections::BTreeMap<String, Vec<String>>,
}

impl Backend for JvmBackend {
    type State = JvmState;

    fn lower_file(
        &self,
        checked: CheckedFile<'_>,
        stem: &str,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let mut outputs = Vec::new();
        let file = checked.file;
        let info = checked.info;
        let syms = checked.symbols;
        let module_name = checked.module_name;

        let emit_opts = shipping_emit_options(stem, module_name, self.class_major, self.cp.clone())
            .with_jvm_default(self.jvm_default)
            .with_lambdas(self.lambdas);

        // Lower the checked file to the backend-agnostic IR, then emit JVM bytecode from it.
        // (The legacy direct AST emitter has been removed — IR is the sole JVM codegen path.)
        let facade_name = file_class_name(stem, file.package.as_deref());
        let runtime = crate::jvm::jvm_libraries::JvmLibraries::new(self.cp.clone());
        let lower_bail = std::cell::RefCell::new(String::new());
        let Some(mut ir) = crate::ir_lower::lower_file_at_reporting(
            file,
            checked.file_index,
            info,
            syms,
            &runtime,
            &lower_bail,
        ) else {
            crate::trace_compiler!("lower", "bail: {}", lower_bail.borrow());
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        // The shared post-lowering pass pipeline (see `run_backend_passes`); an unlowerable shape →
        // diagnose and skip the file rather than miscompile.
        let mut continuation_metadata = crate::jvm::suspend::ContinuationMetadataMap::default();
        if let Err(reason) = run_backend_passes_with_metadata(
            &mut ir,
            file,
            &facade_name,
            module_name,
            syms,
            &mut continuation_metadata,
        ) {
            let what = match reason {
                SkipReason::ValueClasses => "value-class",
                SkipReason::Suspend => "suspend-function",
                SkipReason::Bridges => "bridge-method",
            };
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("krusty: this {what} shape is not yet supported by the IR backend"),
            );
            return outputs;
        }
        // `-Xno-param-assertions` is applied to the lowered IR, after the pass pipeline and before
        // emission, so every consumer of the guards — the guards themselves and the debug tables
        // whose start offsets are measured past them — sees the same thing.
        if !self.param_assertions {
            crate::jvm::ir_emit::strip_param_assertions(&mut ir);
        }
        let emit_opts = emit_opts.with_param_assertions(self.param_assertions);
        let metadata = facade_package_metadata_with_ir(file, checked.file_index, syms, &ir);
        // `emit_all` returns `None` when the IR uses a JVM-unsupported construct. Inline splice failures
        // are reported separately (via `run.inline_bail`): selected inline calls are required to splice,
        // so those are backend errors to fix rather than silent skips.
        let run = crate::jvm::ir_emit::EmitRun::default();
        let Some(classes) = crate::jvm::ir_emit::emit_all_with_opts_and_metadata(
            &ir,
            &facade_name,
            &*self.cp,
            crate::jvm::ir_emit::EmitMetadata {
                facade: metadata.as_ref(),
                continuations: &continuation_metadata,
            },
            &emit_opts,
            &run,
            syms,
        ) else {
            if let Some(reason) = run.inline_bail() {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: JVM backend inline error: {reason}"),
                );
                return outputs;
            }
            if let Some(reason) = run.emit_error() {
                diags.error(crate::diag::Span::new(0, 0), reason);
                return outputs;
            }
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        for (internal, bytes) in classes {
            outputs.push((format!("{internal}.class"), bytes));
        }

        // Record the file facade (`<File>Kt`) for the `.kotlin_module` mapping when the file has
        // top-level functions/props — or TYPEALIASES: an alias-only facade is still emitted (its
        // `Package.typeAlias` records live there) and kotlinc lists it.
        let has_facade_members = file
            .decls
            .iter()
            .any(|&d| matches!(file.decl(d), Decl::Fun(_) | Decl::Property(_)))
            || !file.type_alias_fun.is_empty();
        if has_facade_members {
            let facade = facade_name
                .rsplit('/')
                .next()
                .unwrap_or(&facade_name)
                .to_string();
            state
                .module_packages
                .entry(file.package.clone().unwrap_or_default())
                .or_default()
                .push(facade);
        }
        outputs
    }

    fn finalize(&self, state: JvmState, module_name: &str) -> Vec<Artifact> {
        // META-INF/<module>.kotlin_module — maps packages to their file-facade classes so Kotlin
        // consumers can resolve top-level declarations from the compiled module.
        // kotlinc writes the module file even for a CLASS-ONLY module (empty parts list), so emit
        // it unconditionally — omitting it byte-diverges the artifact set from the reference
        // compiler.
        let packages: Vec<(String, Vec<String>)> = state.module_packages.into_iter().collect();
        let module_bytes = crate::metadata::module::build_kotlin_module(&packages);
        vec![(
            format!("META-INF/{module_name}.kotlin_module"),
            module_bytes,
        )]
    }
}

/// The facade's `@kotlin.Metadata` (`k = 2`, file facade), recording every top-level function —
/// plain and EXTENSION, suspend and INLINE included — with its LOGICAL source signature. The physical
/// descriptor alone cannot express an extension's receiver (it is just the first JVM parameter), a
/// suspend fn's source shape (the CPS form appends a `Continuation`), or an inline fn's contract and
/// reified type parameters, so a SEPARATE compilation reading this facade from the classpath needs
/// the metadata to resolve those calls — kotlinc always writes it. An inline fn carries the
/// `IS_INLINE` flag, its type-parameter table (with `reified`), its erased `JvmMethodSignature`,
/// and its decoded contract (`Function.contract`, field 32). `None` when the file declares no
/// recordable top-level function (the facade is emitted bare, as before).
/// Build facade metadata from the POST-PASS IR. A top-level function with a
/// value-class parameter/return realizes as a MANGLED method (`taggedOnly-rnqsQGE`) with the erased
/// descriptor — neither derivable from the declared record — so its `JvmMethodSignature` (name +
/// desc) is recovered from the value-class pass's declared-sig table. Annotation applications also
/// require IR so metadata consumes complete frontend-checked values instead of source syntax.
pub fn facade_package_metadata_with_ir(
    file: &File,
    file_index: u32,
    syms: &FrontendSymbols,
    ir: &crate::ir::IrFile,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    facade_package_metadata_inner(file, file_index, syms, Some(ir))
}

fn facade_package_metadata_inner(
    file: &File,
    file_index: u32,
    syms: &FrontendSymbols,
    ir: Option<&crate::ir::IrFile>,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    let mut metas: Vec<crate::metadata::builder::FnMeta> = Vec::new();
    for (decl_order, &d) in file.decls.iter().enumerate() {
        let Decl::Fun(f) = file.decl(d) else { continue };
        // The decl's collected signature: a plain fn under `funs[name]`, an extension under
        // `ext_funs[name][semantic receiver]` — matched by source decl id so overloads can't mix.
        let (sig, receiver) = if f.receiver.is_some() {
            let Some((recv, sig)) = syms.ext_funs.get(&f.name).and_then(|families| {
                families.iter().find_map(|(recv, sigs)| {
                    sigs.iter()
                        .find(|s| s.source_decl == Some(d) && s.source_file == Some(file_index))
                        .map(|s| (*recv, s))
                })
            }) else {
                continue;
            };
            (sig, Some(recv))
        } else {
            let Some(sig) = syms.funs.get(&f.name).and_then(|sigs| {
                sigs.iter()
                    .find(|s| s.source_decl == Some(d) && s.source_file == Some(file_index))
            }) else {
                continue;
            };
            (sig, None)
        };
        // The DECLARED receiver for the metadata record (`Refinement<T, R>`, type-parameter
        // arguments included) — the family key is erased (`Refinement`), and a reader unifying a
        // generic call's receiver against it binds the type parameters (`R = String`).
        let receiver = receiver.map(|recv| sig.source_receiver.unwrap_or(recv));
        // The metadata records the DECLARED signature, so a parameter or return written as a type
        // PARAMETER must stay one — the builder maps a `Ty::TyParam` to a `Type.type_parameter`
        // reference, which is how a reader binds `T` from the arguments at a call site. `sig.params`
        // /`sig.ret` are erased: an UNBOUNDED `T` erases to `Any` and survives as a type parameter by
        // accident, but a BOUNDED `<T : Comparable<T>>` erases to the bound, and recording that
        // concrete class made `clampMax(10, 7)` read back as returning `Comparable`, not `Int`.
        // `generic_sig` is the same declaration resolved against the SYMBOLIC type parameters; prefer
        // it, and fall back to the erased form for a non-generic function (which has none).
        let generic = sig.generic_sig.as_ref();
        let declared_params = generic.map_or(&sig.params, |g| &g.params);
        let declared_ret = generic.map_or(sig.ret, |g| g.ret);
        let params: Vec<_> = sig
            .param_names
            .iter()
            .cloned()
            .zip(declared_params.iter().copied())
            .collect();
        // A `suspend fun`'s PHYSICAL method appends a `Continuation` and erases the return; record
        // the emit handle so a reader aligns the logical signature with the CPS method. An inline
        // fn needs the handle too: its erased descriptor (receiver + erased params + erased return)
        // maps the metadata function to its bytecode method. A normal fn's method is name +
        // logical descriptor — recoverable without a recorded handle.
        // A declared TYPE PARAMETER in the signature needs the handle too: the descriptor is not
        // derivable from the proto types (`T` erases to its bound, which the record does not name), so
        // without it kotlin-reflect cannot tell which bytecode method the record describes and reports
        // "several matching members found" for a function that has exactly one.
        let mentions_type_parameter = matches!(declared_ret, Ty::TyParam(..))
            || declared_params
                .iter()
                .any(|parameter| matches!(parameter, Ty::TyParam(..)));
        // kotlinc records NO JvmMethodSignature for a plain inline fn (reified included) — its
        // splicers work from the metadata-derived descriptor, and the spurious handle steered
        // krusty's own consumer away from the `$default` splice route. Suspend fns (the CPS form
        // is not derivable) and type-parameter-mentioning signatures keep the handle.
        let jvm_desc = (f.is_suspend() || mentions_type_parameter).then(|| {
            let mut p = String::new();
            if let Some(r) = receiver {
                p.push_str(&crate::jvm::names::type_descriptor(r));
            }
            for t in &sig.params {
                p.push_str(&crate::jvm::names::type_descriptor(*t));
            }
            let ret_desc = if f.is_suspend() {
                "Ljava/lang/Object;"
            } else {
                &crate::jvm::names::type_descriptor(sig.ret)
            };
            let cont = if f.is_suspend() {
                "Lkotlin/coroutines/Continuation;"
            } else {
                ""
            };
            format!("({p}{cont}){ret_desc}")
        });
        crate::trace_compiler!(
            "metadata",
            "emit facade metadata function={} params={:?} context={} contract={}",
            f.name,
            params,
            sig.context_count,
            sig.contract.is_some(),
        );
        // Shipping metadata consumes the exact checked applications already attached to the IR
        // function. The private no-IR path exists only for metadata-builder unit inspection and emits
        // no user annotations; production callers must never fabricate a partial application.
        let annotations: Vec<crate::ir::AppliedAnnotation> = ir
            .and_then(|ir| ir.top_level_function_fids.get(&d.0).map(|fid| (ir, fid)))
            .and_then(|(ir, fid)| ir.function_annotations.get(fid))
            .map(|annotations| {
                annotations
                    .visible
                    .iter()
                    .chain(&annotations.invisible)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // A value-class-rewritten realization (mangled name + erased descriptor) overrides the
        // derived handle — nothing in the declared record spells either.
        let vc_realization = ir
            .and_then(|ir| ir.top_level_function_fids.get(&d.0).map(|&fid| (ir, fid)))
            .and_then(|(ir, fid)| {
                ir.vc_declared_sigs.get(&fid)?;
                let function = ir.functions.get(fid as usize)?;
                Some((
                    function.name.clone(),
                    crate::jvm::names::method_descriptor(&function.params, function.ret),
                ))
            });
        let (jvm_desc, jvm_name) = match vc_realization {
            Some((physical_name, physical_desc)) => (
                Some(physical_desc),
                (physical_name != f.name).then_some(physical_name),
            ),
            None => (jvm_desc, None),
        };
        metas.push(crate::metadata::builder::FnMeta {
            name: f.name.clone(),
            params,
            decl_order,
            annotations,
            jvm_name,
            ret: declared_ret,
            receiver,
            param_defaults: sig.param_defaults.clone(),
            suspend: f.is_suspend(),
            jvm_desc,
            inline: f.is_inline(),
            operator: f.is_operator(),
            type_params: f
                .type_params
                .iter()
                .map(|tp| (tp.clone(), f.reified_type_params.contains(tp)))
                .collect(),
            semantic_type_params: generic
                .map(|signature| signature.formals.clone())
                .unwrap_or_default(),
            type_param_bounds: generic
                .map(|signature| signature.formal_bounds.clone())
                .unwrap_or_default(),
            // Resolve the contract's source type references once, against this module: a type
            // parameter stays a `Type.type_parameter` reference; a class becomes its internal
            // name. Unresolvable references stay `Source` (the emitter degrades them to `Any`).
            context_count: sig.context_count,
            vararg_index: sig.vararg_index,
            visibility: sig.visibility,
            contract: sig.contract.as_ref().map(|c| {
                // Source-level class name → JVM internal name — the SAME lookup the checker uses
                // (`class_internal_resolver`, shared via `frontend`), so contract types resolve
                // identically on both sides of the metadata boundary.
                let resolve_class = crate::frontend::class_internal_resolver(syms);
                std::sync::Arc::new(c.with_resolved_types(&mut |tref| {
                    if f.type_params.iter().any(|tp| tp == &tref.name) {
                        Some(Ty::ty_param(
                            &tref.name,
                            Ty::nullable(Ty::obj("kotlin/Any")),
                        ))
                    } else {
                        let ty = Ty::obj_name(resolve_class(&tref.name)?);
                        Some(if tref.nullable() {
                            Ty::nullable(ty)
                        } else {
                            ty
                        })
                    }
                }))
            }),
        });
    }
    // Package properties share one metadata list. An extension accessor's leading JVM parameter does
    // not identify receiver-ness, so its semantic receiver is recorded separately; a plain top-level
    // property has no receiver but still needs a declaration record for Kotlin consumers (`::value`,
    // imports, mutability, and its Kotlin type). Accessor descriptors are realization data only.
    let mut prop_metas: Vec<crate::metadata::builder::PropMeta> = Vec::new();
    for (decl_order, &d) in file.decls.iter().enumerate() {
        let Decl::Property(p) = file.decl(d) else {
            continue;
        };
        let (ty, is_var, is_const, has_constant, type_params, receiver, mut accessor_params) =
            if p.receiver.is_some() {
                // Match by declaration, not name: extension properties may share a spelling on
                // different receivers and still denote different declarations.
                let Some(property) = syms.source_extension_property((file_index, d.0)) else {
                    continue;
                };
                (
                    property.ty,
                    property.is_var,
                    false,
                    false,
                    property.formals.clone(),
                    Some(property.receiver),
                    std::iter::once(property.receiver)
                        .chain(property.context_params.iter().copied())
                        .collect::<Vec<_>>(),
                )
            } else {
                let Some(property) = syms.source_props.get(&(file_index, d.0)) else {
                    continue;
                };
                (
                    property.ty,
                    property.is_var,
                    property.is_const,
                    property.compile_time_constant.is_some(),
                    Vec::new(),
                    None,
                    property.context_params.clone(),
                )
            };
        let descriptor_params = accessor_params
            .iter()
            .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
            .collect::<String>();
        let ty_desc = crate::jvm::names::type_descriptor(ty);
        // Kotlin's accessor-name rules (`isTagged` keeps its getter name, `setTagged` for the
        // setter) — the SAME helper the bytecode emitter uses, so the recorded JvmPropertySignature
        // always names a method that exists.
        let getter = (
            crate::jvm::names::property_getter_name(&p.name),
            format!("({descriptor_params}){ty_desc}"),
        );
        let setter = is_var.then(|| {
            accessor_params.push(ty);
            let params = accessor_params
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>();
            (
                crate::jvm::names::property_setter_name(&p.name),
                format!("({params})V"),
            )
        });
        prop_metas.push(crate::metadata::builder::PropMeta {
            name: p.name.clone(),
            ty,
            is_var,
            type_params,
            receiver,
            getter,
            setter,
            is_const,
            has_constant,
            decl_order,
            visibility: p.visibility,
        });
    }
    let package = file
        .package
        .as_deref()
        .unwrap_or_default()
        .replace('.', "/");
    let alias_metas = file
        .type_alias_fun
        .iter()
        .map(|(alias, _, _)| {
            let qualified = if package.is_empty() {
                alias.clone()
            } else {
                format!("{package}/{alias}")
            };
            let (formals, expansion) = syms
                .source_alias_expansions
                .get(&crate::types::type_name(&qualified))
                .unwrap_or_else(|| panic!("frontend did not resolve typealias '{qualified}'"));
            crate::metadata::builder::TypeAliasMeta {
                name: alias.clone(),
                formals: formals.clone(),
                expansion: *expansion,
                visibility: file
                    .type_alias_visibility
                    .get(alias)
                    .copied()
                    .unwrap_or(crate::types::Visibility::Public),
            }
        })
        .collect::<Vec<_>>();
    (!metas.is_empty() || !prop_metas.is_empty() || !alias_metas.is_empty()).then(|| {
        let (d1_bytes, d2) =
            crate::metadata::builder::build_package(&metas, &prop_metas, &alias_metas);
        // `d1` is the protobuf payload with one byte per `char` (the constant pool writes it as
        // modified-UTF-8, which the reader decodes back to the same bytes).
        let d1: String = d1_bytes.iter().map(|&b| b as char).collect();
        crate::jvm::ir_emit::KotlinMetadata {
            k: 2,
            mv: vec![2, 4, 0],
            xi: 48,
            d1: vec![d1],
            d2,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::frontend::{collect_signatures, parse_source_with_detected_features};

    /// Every caller supplies a logical source stem, but module/corpus callers can retain directories
    /// that the CLI has already stripped. The shared constructor must own that normalization so all
    /// emitted `SourceFile` attributes contain the JVM-required simple filename on either path style.
    #[test]
    fn shipping_emit_options_normalize_logical_source_paths() {
        let cp = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let unix = shipping_emit_options("suite/nested/Foo", "main", None, cp.clone());
        let windows = shipping_emit_options("suite\\nested\\Bar", "main", None, cp);

        assert_eq!(unix.source_file.as_deref(), Some("Foo.kt"));
        assert_eq!(windows.source_file.as_deref(), Some("Bar.kt"));
    }

    /// These are not arbitrary emitter unit tests: each claims to compile or survey the bytes that
    /// krusty ships. Pin that architectural boundary so adding a new `EmitOptions` field cannot leave
    /// one of those pipelines on a subtly different artifact shape. A focused differential helper may
    /// mutate the returned options afterward (for example, force metadata despite a bisect env var),
    /// but it must still begin with the complete shared configuration.
    #[test]
    fn shipping_pipelines_do_not_reimplement_emit_options() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "src/bin/survey.rs",
            "tests/common/mod.rs",
            "tests/kotlin_box_ir_jvm_conformance.rs",
        ] {
            let text =
                std::fs::read_to_string(root.join(relative)).expect("read shipping pipeline");
            assert!(
                !text.contains("EmitOptions {"),
                "{relative} must start from jvm::backend::shipping_emit_options instead of duplicating the shipping configuration",
            );
        }
    }

    #[test]
    fn prepare_module_symbols_records_cross_file_facades() {
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(
                "package p\nfun helper(): String = \"OK\"\n\
                 inline operator fun String.unaryMinus(): String = this\n\
                 inline fun <reified T> spliceOnly(value: Any): T? = value as? T\n\
                 val answer: Int = 42",
                &mut diags,
            ),
            parse_source_with_detected_features(
                "package p\nfun box(): String = helper()",
                &mut diags,
            ),
        ];
        let stems = vec!["A".to_string(), "B".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);

        prepare_module_symbols(&files, &stems, &mut syms);

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(
            syms.fn_facades.get("helper").map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        assert_eq!(
            syms.prop_facades
                .get("answer")
                .map(|(facade, _, _, _)| facade.render()),
            Some("p/AKt".to_string())
        );
        let extension = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function)
                        if function.name == "unaryMinus" && function.receiver.is_some()
                )
            })
            .expect("source extension declaration");
        assert_eq!(
            syms.fn_facades_by_decl
                .get(&(0, extension.0))
                .map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        let splice_only = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function) if function.name == "spliceOnly"
                )
            })
            .expect("splice-only source declaration");
        assert!(
            syms.fn_facade_is_explicitly_unemitted(0, splice_only.0),
            "registration must preserve the negative outcome so checker-only absence is distinct"
        );
        assert!(!syms.fn_facades_by_decl.contains_key(&(0, splice_only.0)));
    }

    #[test]
    fn facade_metadata_round_trips_plain_top_level_properties() {
        let mut diagnostics = DiagSink::new();
        let file = parse_source_with_detected_features(
            "package a\nvar topLevel: Int = 42",
            &mut diagnostics,
        );
        let files = vec![file];
        let mut symbols = collect_signatures(&files, &mut diagnostics);
        prepare_module_symbols(&files, &["A".to_string()], &mut symbols);

        let metadata = facade_package_metadata_inner(&files[0], 0, &symbols, None)
            .expect("a package property requires facade metadata");
        let decoded = crate::jvm::metadata::decode_metadata(
            &metadata.d1,
            &metadata.d2,
            Some(metadata.k),
            "a/AKt",
            None,
            &[],
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let [property] = decoded.package_properties.as_ref() else {
            panic!(
                "expected one package property, got {:?}",
                decoded.package_properties
            );
        };
        assert_eq!(property.name, "topLevel");
        assert!(property.is_var);
        assert_eq!(
            property.ret_class,
            Some(crate::types::type_name("kotlin/Int"))
        );
    }

    /// JVM post-lowering passes must run through `run_backend_passes`.
    #[test]
    fn backend_passes_are_only_called_via_run_backend_passes() {
        // token that marks a CALL of the pass → files allowed to contain it (the defining module's
        // internal/recursive uses, and the shared pipeline in this file).
        let rules: &[(&str, &[&str])] = &[
            (
                "lower_companion_properties(",
                &["src/jvm/companion.rs", "src/jvm/backend.rs"],
            ),
            (
                "elide_default_property_stores(",
                &["src/jvm/property_storage.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_value_classes(",
                &["src/jvm/value_classes.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_suspend(",
                &["src/jvm/suspend.rs", "src/jvm/backend.rs"],
            ),
            (
                "mark_must_inline_lambdas(",
                &["src/jvm/ir_emit.rs", "src/jvm/backend.rs"],
            ),
            (
                "reparent_lambda_impls(",
                &["src/jvm/ir_emit.rs", "src/jvm/backend.rs"],
            ),
            (
                "run_enabled(",
                &["src/plugins/mod.rs", "src/jvm/backend.rs"],
            ),
            (
                "derive_bridges(",
                &["src/jvm/bridges.rs", "src/jvm/backend.rs"],
            ),
            ("apply_collection_bridge_barriers(", &["src/jvm/backend.rs"]),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for dir in ["src", "tests"] {
            visit(&root.join(dir), &mut |path, text| {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                for (token, allowed) in rules {
                    if text.contains(token) && !allowed.contains(&rel.as_str()) {
                        offenders.push(format!("{rel}: calls `{token}…)` directly"));
                    }
                }
            });
        }
        assert!(
            offenders.is_empty(),
            "backend passes must go through jvm::backend::run_backend_passes (so a new pass lands \
             in every pipeline by construction), but:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn common_lowering_leaves_jvm_storage_choices_to_backend() {
        let lowering = include_str!("../ir_lower.rs");
        assert!(!lowering.contains("lower_companion_properties"));
        assert!(!lowering.contains("mark_jvm_companion_hoisted_static"));
        assert!(!lowering.contains("jvm_default"));
        assert!(!lowering.contains("fieldInitializerOptimization"));
    }

    #[test]
    fn common_lowering_leaves_bridge_barriers_to_backends() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ir_lower.rs");
        let text = std::fs::read_to_string(path).expect("read common lowering");
        let offenders = text
            .lines()
            .filter(|line| {
                line.contains("type_safe_barrier") && !line.contains("type_safe_barrier: false")
            })
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "common lowering must not assign backend bridge barriers:\n{}",
            offenders.join("\n")
        );
    }

    fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                visit(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    f(&p, &text);
                }
            }
        }
    }
}
