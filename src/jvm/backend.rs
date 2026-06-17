//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::ast::{Decl, File};
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::jvm::names::file_class_name;
use crate::resolve::{SymbolTable, TypeInfo};

pub struct JvmBackend;

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
        let Some(ir) = crate::ir_lower::lower_file(file, info, syms) else {
            diags.error(crate::diag::Span::new(0, 0), "krusty: this construct is not yet supported by the IR backend".to_string());
            return outputs;
        };
        // `emit_all` returns `None` when the IR uses a JVM-unsupported construct (e.g. a function type
        // above the fixed-arity `Function0..22` the JVM stdlib provides) — skip rather than miscompile.
        let Some(classes) = crate::jvm::ir_emit::emit_all(&ir, &facade_name) else {
            diags.error(crate::diag::Span::new(0, 0), "krusty: this construct is not yet supported by the IR backend".to_string());
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
            let facade = facade_name.rsplit('/').next().unwrap_or(&facade_name).to_string();
            state.module_packages.entry(file.package.clone().unwrap_or_default()).or_default().push(facade);
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
        vec![(format!("META-INF/{module_name}.kotlin_module"), module_bytes)]
    }
}
