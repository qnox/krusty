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
/// 2. `lower_value_classes` — realize `@JvmInline value class`es as their unboxed underlying type
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected).
/// 3. `lower_suspend` — realize `suspend fun`s as their continuation-passing-style ABI.
/// 4. `mark_must_inline_lambdas` — drop the dead standalone impl of a must-inline call's
///    (`require`/`check`) message lambda; it is spliced at the call site.
/// 5. `reparent_lambda_impls` — a lambda impl method must be a member of the CLASS whose code emits
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
    let resolve_class_name = |name: &str| syms.class_names.get(name).map(|name| name.render());
    crate::plugins::run_enabled(
        ir,
        file,
        module_name,
        &resolve_class_name,
        jvm_plugin_type_descriptor,
    );
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
    if !crate::jvm::suspend::lower_suspend(ir, facade) {
        return Err(SkipReason::Suspend);
    }
    crate::jvm::ir_emit::mark_must_inline_lambdas(ir);
    crate::jvm::ir_emit::reparent_lambda_impls(ir);
    Ok(())
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

pub fn prepare_module_symbols(files: &[File], stems: &[String], syms: &mut FrontendSymbols) {
    if files.len() <= 1 {
        return;
    }

    let mut fns: Vec<(u32, u32, String, String)> = Vec::new();
    let mut props: Vec<(String, String)> = Vec::new();
    for (i, (file, stem)) in files.iter().zip(stems).enumerate() {
        let facade = file_class_name(stem, file.package.as_deref());
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) if f.receiver.is_none() && !f.is_inline => {
                    fns.push((i as u32, d.0, f.name.clone(), facade.clone()))
                }
                Decl::Property(p) if p.receiver.is_none() => {
                    props.push((p.name.clone(), facade.clone()))
                }
                _ => {}
            }
        }
    }

    for (file_index, decl_id, name, facade) in fns {
        syms.fn_facades_by_decl
            .insert((file_index, decl_id), type_name(&facade));
        syms.fn_facades.insert(name, type_name(&facade));
    }
    for (name, facade) in props {
        if let Some(&(ty, is_var, is_const)) = syms.props.get(&name) {
            syms.prop_facades
                .insert(name, (type_name(&facade), ty, is_var, is_const));
        }
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

        // Per-file emit config: the class version (`-jvm-target`) and the `SourceFile` name (the origin
        // `.kt`; kotlinc uses the simple name, reconstructed from the stem — directories already
        // stripped). Threaded explicitly into emission so every class (incl. synthetics) carries it.
        let emit_opts = crate::jvm::ir_emit::EmitOptions {
            class_major: self.class_major,
            source_file: Some(format!("{stem}.kt")),
            // kotlinc records `classModuleName` in @Metadata unless the module is the default `main`.
            module_name: (module_name != "main").then(|| module_name.to_string()),
            // Opt-in (WIP): compute + emit `@Metadata` for supported shapes. Off unless requested, so
            // the default emit is unchanged (an unverified payload breaks kotlin-reflect).
            emit_class_metadata: std::env::var_os("KRUSTY_EMIT_CLASS_METADATA").is_some(),
        };

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
        if let Err(reason) = run_backend_passes(&mut ir, file, &facade_name, module_name, syms) {
            let what = match reason {
                SkipReason::ValueClasses => "value-class",
                SkipReason::Suspend => "suspend-function",
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
        let Some(classes) = crate::jvm::ir_emit::emit_all_with_opts(
            &ir,
            &facade_name,
            &*self.cp,
            metadata.as_ref(),
            &emit_opts,
            &run,
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

/// The facade's `@kotlin.Metadata` (`k = 2`, file facade), recording every NON-INLINE top-level
/// function — plain and EXTENSION, suspend included — with its LOGICAL source signature. The physical
/// descriptor alone cannot express an extension's receiver (it is just the first JVM parameter) or a
/// suspend fn's source shape (the CPS form appends a `Continuation`), so a SEPARATE compilation
/// reading this facade from the classpath needs the metadata to resolve those calls — kotlinc always
/// writes it. Inline functions stay unrecorded: the builder carries no `IS_INLINE` flag yet, and
/// advertising an inline fn as a plain callable would mis-resolve it. `None` when the file declares
/// no recordable top-level function (the facade is emitted bare, as before).
pub fn facade_package_metadata(
    file: &File,
    file_index: u32,
    syms: &FrontendSymbols,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    let mut metas: Vec<crate::metadata::builder::FnMeta> = Vec::new();
    for &d in &file.decls {
        let Decl::Fun(f) = file.decl(d) else { continue };
        if f.is_inline {
            continue;
        }
        // The decl's collected signature: a plain fn under `funs[name]`, an extension under
        // `ext_funs[(erased receiver, name)]` — matched by source decl id so overloads can't mix.
        let (sig, receiver) = if f.receiver.is_some() {
            let Some((recv, sig)) = syms.ext_funs.iter().find_map(|((recv, name), sigs)| {
                (*name == f.name)
                    .then(|| {
                        sigs.iter()
                            .find(|s| s.source_decl == Some(d) && s.source_file == Some(file_index))
                            .map(|s| (*recv, s))
                    })
                    .flatten()
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
        // the emit handle so a reader aligns the logical signature with the CPS method. A normal
        // fn's method is name + logical descriptor — recoverable without a recorded handle.
        let jvm_desc = f.is_suspend.then(|| {
            let mut p = String::new();
            if let Some(r) = receiver {
                p.push_str(&crate::jvm::names::type_descriptor(r));
            }
            for t in &sig.params {
                p.push_str(&crate::jvm::names::type_descriptor(*t));
            }
            format!("({p}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;")
        });
        metas.push(crate::metadata::builder::FnMeta {
            name: f.name.clone(),
            params,
            ret: sig.ret,
            receiver,
            param_fun_recvs,
            param_defaults: sig.param_defaults.clone(),
            suspend: f.is_suspend,
            jvm_desc,
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
        let Some(((recv, _), prop_sig)) = syms
            .ext_props
            .iter()
            .find(|(_, sig)| sig.source == (file_index, d.0))
        else {
            continue;
        };
        let (ty, is_var) = (prop_sig.ty, prop_sig.is_var);
        let recv = *recv;
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

    #[test]
    fn prepare_module_symbols_records_cross_file_facades() {
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(
                "package p\nfun helper(): String = \"OK\"\nval answer: Int = 42",
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
    }

    /// The post-lowering JVM pass pipeline (plugins → value-classes → suspend → must-inline marks →
    /// lambda reparenting) must run through `run_backend_passes` everywhere — every hand-replicated
    /// copy is a site where a NEW pass silently goes missing (twice this produced false-green test
    /// runs: IllegalAccessError miscompiles the gate never saw). This test bans direct calls to the
    /// individual passes outside their defining module and the shared pipeline itself.
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
