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
/// 2. `derive_bridges` — synthesize the `ACC_BRIDGE` methods an override needs to be reachable through
///    a supertype's erased descriptor. A bridge is a JVM realization of an override, not a Kotlin
///    declaration, so lowering records only the declarations and this pass derives the bridges.
/// 3. `apply_collection_bridge_barriers` — attach JVM collection bridge semantics.
/// 4. `lower_value_classes` — realize `@JvmInline value class`es as their unboxed underlying type
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected).
/// 5. `lower_suspend` — realize `suspend fun`s as their continuation-passing-style ABI.
/// 6. `mark_must_inline_lambdas` — drop the dead standalone impl of a must-inline call's
///    (`require`/`check`) message lambda; it is spliced at the call site.
/// 7. `reparent_lambda_impls` — a lambda impl method must be a member of the CLASS whose code emits
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
    let resolve_class_name = |name: &str| syms.class_names.get(name).map(|name| name.render());
    crate::plugins::run_enabled(
        ir,
        file,
        module_name,
        &resolve_class_name,
        jvm_plugin_type_descriptor,
    );
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
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<crate::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend {
            cp,
            class_major: None,
        }
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
/// inherits it. A caller that wants the pre-class-metadata bytes uses `EmitOptions::default()`.
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
        // Compute + emit each class's own `@Metadata`. Without it a krusty-compiled CLASS is
        // unreadable BY KRUSTY: the facade metadata describes top-level declarations only, so a
        // second compilation sees no constructor/member parameter names (named arguments) and no
        // `operator` marks (destructuring). A shape `build_class_metadata` has not verified against
        // kotlinc declines individually and emits nothing, so this cannot write an unverified
        // payload. `KRUSTY_NO_CLASS_METADATA` restores the facade-only output for bisecting.
        emit_class_metadata: std::env::var_os("KRUSTY_NO_CLASS_METADATA").is_none(),
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
    let mut props: Vec<(String, String)> = Vec::new();
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
                    let emitted = syms.source_fn_has_callable_body(file, i as u32, d, f);
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
                    props.push((p.name.clone(), facade.clone()))
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
    for (name, facade) in props {
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

        let emit_opts = shipping_emit_options(stem, module_name, self.class_major, self.cp.clone());

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
        let metadata = facade_package_metadata(file, checked.file_index, syms);
        // `emit_all` returns `None` when the IR uses a JVM-unsupported construct. Inline splice failures
        // are reported separately (via `run.inline_bail`): selected inline calls are required to splice,
        // so those are backend errors to fix rather than silent skips.
        let run = crate::jvm::ir_emit::EmitRun::default();
        let Some(classes) = crate::jvm::ir_emit::emit_all_with_opts_and_metadata(
            &ir,
            &facade_name,
            &*self.cp,
            metadata.as_ref(),
            &emit_opts,
            &run,
            &continuation_metadata,
        ) else {
            if let Some(reason) = run.inline_bail() {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: JVM backend inline error: {reason}"),
                );
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
        // top-level functions/props.
        let has_facade_members = file
            .decls
            .iter()
            .any(|&d| matches!(file.decl(d), Decl::Fun(_) | Decl::Property(_)));
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
        if state.module_packages.is_empty() {
            return Vec::new();
        }
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
pub fn facade_package_metadata(
    file: &File,
    file_index: u32,
    syms: &FrontendSymbols,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    let mut metas: Vec<crate::metadata::builder::FnMeta> = Vec::new();
    for &d in &file.decls {
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
        let params: Vec<_> = sig
            .param_names
            .iter()
            .cloned()
            .zip(sig.params.iter().copied())
            .collect();
        // Per-parameter receiver function types (`Recv.(…) -> R`): the reader recovers lambda `this`
        // binding from the `@kotlin.ExtensionFunctionType` mark this drives.
        let param_fun_recvs: Vec<Option<Ty>> = sig
            .lambda_recv
            .iter()
            .enumerate()
            .map(|(i, &is_recv)| {
                is_recv
                    .then(|| {
                        sig.lambda_param_types
                            .get(i)
                            .and_then(|t| t.first())
                            .copied()
                    })
                    .flatten()
            })
            .collect();
        // A `suspend fun`'s PHYSICAL method appends a `Continuation` and erases the return; record
        // the emit handle so a reader aligns the logical signature with the CPS method. An inline
        // fn needs the handle too: its erased descriptor (receiver + erased params + erased return)
        // maps the metadata function to its bytecode method. A normal fn's method is name +
        // logical descriptor — recoverable without a recorded handle.
        let jvm_desc = (f.is_suspend() || f.is_inline()).then(|| {
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
        metas.push(crate::metadata::builder::FnMeta {
            name: f.name.clone(),
            params,
            ret: sig.ret,
            receiver,
            param_fun_recvs,
            param_defaults: sig.param_defaults.clone(),
            suspend: f.is_suspend(),
            jvm_desc,
            inline: f.is_inline(),
            type_params: f
                .type_params
                .iter()
                .map(|tp| (tp.clone(), f.reified_type_params.contains(tp)))
                .collect(),
            // Resolve the contract's source type references once, against this module: a type
            // parameter stays a `Type.type_parameter` reference; a class becomes its internal
            // name. Unresolvable references stay `Source` (the emitter degrades them to `Any`).
            context_count: sig.context_count,
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
    // Extension PROPERTIES (`val String.doubled`): the accessor is a static `getName(Recv)` whose
    // descriptor cannot mark receiver-ness — only `Property.receiver_type` in the metadata does.
    // Plain top-level properties keep resolving through the facade's static fields (unrecorded, as
    // before).
    let mut prop_metas: Vec<crate::metadata::builder::PropMeta> = Vec::new();
    for &d in &file.decls {
        let Decl::Property(p) = file.decl(d) else {
            continue;
        };
        if p.receiver.is_none() {
            continue;
        }
        // Match by the SOURCE declaration, not the name — two extension properties may share a name
        // on different receivers (`val String.x` / `val Int.x`); the source key picks this decl's.
        let Some(prop_sig) = syms.source_extension_property((file_index, d.0)) else {
            continue;
        };
        let (ty, is_var) = (prop_sig.ty, prop_sig.is_var);
        let recv = prop_sig.receiver;
        let cap = {
            let mut c = p.name.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        };
        let recv_desc = crate::jvm::names::type_descriptor(recv);
        let ty_desc = crate::jvm::names::type_descriptor(ty);
        let getter = (format!("get{cap}"), format!("({recv_desc}){ty_desc}"));
        let setter = is_var.then(|| (format!("set{cap}"), format!("({recv_desc}{ty_desc})V")));
        prop_metas.push(crate::metadata::builder::PropMeta {
            name: p.name.clone(),
            ty,
            is_var,
            receiver: Some(recv),
            getter,
            setter,
        });
    }
    (!metas.is_empty() || !prop_metas.is_empty()).then(|| {
        let (d1_bytes, d2) = crate::metadata::builder::build_package(&metas, &prop_metas);
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
                 inline fun <reified T> spliceOnly(): String = \"x\"\n\
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

    /// JVM post-lowering passes must run through `run_backend_passes`.
    #[test]
    fn backend_passes_are_only_called_via_run_backend_passes() {
        // token that marks a CALL of the pass → files allowed to contain it (the defining module's
        // internal/recursive uses, and the shared pipeline in this file).
        let rules: &[(&str, &[&str])] = &[
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
