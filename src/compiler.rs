//! Compiler orchestration.

use crate::ast::File;
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::frontend::{check_file, CheckedFile};
use crate::resolve::SymbolTable;

/// Check each parsed file and hand it to the backend.
pub fn compile<B: Backend>(
    files: &[File],
    stems: &[String],
    syms: &mut SymbolTable,
    backend: &B,
    module_name: &str,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    let mut outputs = Vec::new();
    let mut state = B::State::default();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        let info = check_file(file, syms, diags);
        if diags.has_errors() {
            continue;
        }
        outputs.extend(backend.lower_file(
            CheckedFile {
                file,
                info: &info,
                symbols: syms,
            },
            &stems[i],
            &mut state,
            diags,
        ));
    }
    if !diags.has_errors() {
        outputs.extend(backend.finalize(state, module_name));
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Artifact;
    use crate::frontend::{collect_signatures, parse_source_with_detected_features};

    struct RecordingBackend;

    impl Backend for RecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            checked: CheckedFile<'_>,
            stem: &str,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            *state += checked.file.decls.len();
            vec![(format!("{stem}.out"), Vec::new())]
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![("module.out".to_string(), state.to_string().into_bytes())]
        }
    }

    #[test]
    fn compiler_orchestrates_frontend_then_backend() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): String = \"OK\"",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].0, "Main.out");
        assert_eq!(outputs[1], ("module.out".to_string(), b"1".to_vec()));
    }

    #[test]
    fn compiler_does_not_lower_after_frontend_error() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): Int = \"no\"",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(diags.has_errors());
        assert!(outputs.is_empty());
    }
}
