//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::ast::{Decl, File};
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::jvm::emit::{emit_class, emit_file, file_class_name};
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

        // Each top-level `class` becomes its own `.class` file.
        let facade_name = file_class_name(stem, file.package.as_deref());
        for &d in &file.decls {
            if let Decl::Class(c) = file.decl(d) {
                let internal = match file.package.as_deref() {
                    Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), c.name),
                    _ => c.name.clone(),
                };
                let (bytes, extra) = emit_class(c, file, info, &internal, &facade_name, syms, diags);
                outputs.push((format!("{internal}.class"), bytes));
                for (name, eb) in extra {
                    outputs.push((format!("{name}.class"), eb));
                }
            }
        }

        // The file facade (`<File>Kt`) is emitted only if the file has top-level functions/props.
        let has_facade_members = file
            .decls
            .iter()
            .any(|&d| matches!(file.decl(d), Decl::Fun(_) | Decl::Property(_)));
        if has_facade_members {
            let internal = file_class_name(stem, file.package.as_deref());
            let (bytes, extra) = emit_file(file, info, syms, &internal, diags);
            if !diags.has_errors() {
                let facade = internal.rsplit('/').next().unwrap_or(&internal).to_string();
                state.module_packages.entry(file.package.clone().unwrap_or_default()).or_default().push(facade);
                outputs.push((format!("{internal}.class"), bytes));
                for (name, eb) in extra {
                    outputs.push((format!("{name}.class"), eb));
                }
            }
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
