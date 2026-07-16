//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::ast::{Decl, File};
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::jvm::names::{file_class_name, type_descriptor};
use crate::resolve::{SymbolTable, TypeInfo};

/// The JVM backend holds the shared classpath (`Rc`, same instance as `JvmLibraries`) so the emitter
/// can read inline-function bodies for the bytecode inliner.
pub struct JvmBackend {
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<crate::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend { cp }
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
        file: &File,
        info: &TypeInfo,
        syms: &SymbolTable,
        stem: &str,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let mut outputs = Vec::new();

        // Lower the checked file to the backend-agnostic IR, then emit JVM bytecode from it.
        // (The legacy direct AST emitter has been removed — IR is the sole JVM codegen path.)
        let facade_name = file_class_name(stem, file.package.as_deref());
        let runtime = crate::jvm::jvm_libraries::JvmLibraries::new(self.cp.clone());
        let Some(mut ir) = crate::ir_lower::lower_file(file, info, syms, &runtime) else {
            crate::trace_compiler!("lower", "bail: {}", crate::ir_lower::lower_bail_reason());
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        // Compiler-extension plugins (kotlinx.serialization): synthesize declarations from the file's
        // annotations before the JVM IR transforms. No-op when no trigger annotation is present.
        crate::plugins::run_enabled(&mut ir, file);
        // JVM-only IR→IR transform: realize `@JvmInline value class`es as their unboxed underlying type
        // (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected). A
        // value-class shape it can't yet lower → skip the file (same as any unsupported construct).
        let vc_module = crate::module_symbols::ModuleSymbols::new(syms);
        let vc_resolver = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            &*syms.libraries,
            &vc_module,
            &[],
        );
        if !crate::jvm::value_classes::lower_value_classes(&mut ir, &vc_resolver) {
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this value-class shape is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        }
        // JVM-only IR→IR transform: realize `suspend fun`s as their continuation-passing-style ABI
        // (the IR keeps them as plain functions so JS / other backends are unaffected). A suspend shape
        // it can't yet lower → skip the file rather than miscompile.
        if !crate::jvm::suspend::lower_suspend(&mut ir, &facade_name) {
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this suspend-function shape is not yet supported by the IR backend"
                    .to_string(),
            );
            return outputs;
        }
        // Drop the dead standalone impl of a must-inline call's (`require`/`check`) message lambda — it is
        // spliced at the call site, so emitting it would only force a spurious facade class.
        crate::jvm::ir_emit::mark_must_inline_lambdas(&mut ir);
        // A lambda impl method must be a member of the CLASS whose code emits its `invokedynamic` —
        // the impl is PRIVATE (kotlinc's placement), so a cross-class handle would be an
        // IllegalAccessError. Lowering attaches impls per `cur_class`, which misses code that ends up
        // in a class only later: enum-entry constructor arguments (lowered class-less, emitted in the
        // enum's `<clinit>`) and suspend-lambda state machines (bodies moved into the machine class by
        // `lower_suspend`). Reparent those impls after all IR→IR transforms, before emit.
        crate::jvm::ir_emit::reparent_lambda_impls(&mut ir);
        // `@kotlin.Metadata` for the facade: each top-level `suspend fun` is recorded with `IS_SUSPEND`
        // and its LOGICAL signature, so another krusty/kotlinc compilation resolves a call to it (a
        // suspend fn's physical method is `Object foo(…, Continuation)` — only `@Metadata` distinguishes
        // it). Emitted only when the file has top-level suspend functions; non-suspend facades resolve
        // fine from their physical descriptors, so they keep emitting no `@Metadata` (unchanged).
        let susp_metas: Vec<crate::metadata::builder::FnMeta> = file
            .decls
            .iter()
            .filter_map(|&d| {
                let Decl::Fun(f) = file.decl(d) else {
                    return None;
                };
                if !f.is_suspend || f.receiver.is_some() || f.is_inline {
                    return None;
                }
                let sig = syms.funs.get(&f.name)?.iter().find(|s| s.is_suspend)?;
                let params: Vec<_> = sig
                    .param_names
                    .iter()
                    .cloned()
                    .zip(sig.params.iter().copied())
                    .collect();
                // Physical CPS descriptor: the logical params then a trailing `Continuation`, returning
                // the erased `Object`.
                let pdescs: String = sig.params.iter().map(|t| type_descriptor(*t)).collect();
                let jvm_desc =
                    format!("({pdescs}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;");
                Some(crate::metadata::builder::FnMeta {
                    name: f.name.clone(),
                    params,
                    ret: sig.ret,
                    receiver: None,
                    param_fun_recvs: Vec::new(),
                    param_defaults: Vec::new(),
                    suspend: true,
                    jvm_desc: Some(jvm_desc),
                })
            })
            .collect();
        let metadata = (!susp_metas.is_empty()).then(|| {
            let (d1_bytes, d2) = crate::metadata::builder::build_package(&susp_metas, &[]);
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
        });
        // `emit_all` returns `None` when the IR uses a JVM-unsupported construct. Inline splice failures
        // are reported separately: selected inline calls are required to splice, so those are backend
        // errors to fix rather than silent skips.
        let Some(classes) =
            crate::jvm::ir_emit::emit_all(&ir, &facade_name, &*self.cp, metadata.as_ref())
        else {
            if let Some(reason) = crate::jvm::ir_emit::inline_bail_reason() {
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
