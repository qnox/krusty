use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::frontend::CheckedFile;
use crate::runtime::TargetRuntime;

pub struct JsBackend<R> {
    runtime: R,
}

impl<R> JsBackend<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }
}

impl<R> Backend for JsBackend<R>
where
    R: TargetRuntime,
{
    type State = ();

    fn lower_file(
        &self,
        checked: CheckedFile<'_>,
        stem: &str,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let Some(ir) = crate::ir_lower::lower_file_at(
            checked.file,
            checked.file_index,
            checked.info,
            checked.symbols,
            &self.runtime,
        ) else {
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return Vec::new();
        };
        vec![(format!("{stem}.js"), super::emit_file(&ir).into_bytes())]
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::Artifact;
    use crate::diag::DiagSink;
    use crate::frontend::{collect_signatures_with_cp, parse_source_with_detected_features};
    use crate::libraries::EmptySymbolSource;

    fn compile_js_sources(sources: &[(&str, &str)]) -> (Vec<Artifact>, DiagSink) {
        let mut diags = DiagSink::new();
        let files = sources
            .iter()
            .map(|(_, src)| parse_source_with_detected_features(src, &mut diags))
            .collect::<Vec<_>>();
        let stems = sources
            .iter()
            .map(|(stem, _)| (*stem).to_string())
            .collect::<Vec<_>>();
        let mut syms = collect_signatures_with_cp(&files, Box::new(EmptySymbolSource), &mut diags);
        let outputs = crate::compiler::compile(
            &files,
            &stems,
            &mut syms,
            &super::JsBackend::new(EmptySymbolSource),
            "main",
            &mut diags,
        );
        (outputs, diags)
    }

    fn diagnostic_messages(diags: &DiagSink) -> Vec<&str> {
        diags.diags.iter().map(|diag| diag.msg.as_str()).collect()
    }

    #[test]
    fn js_backend_runs_through_common_compiler_driver() {
        let (outputs, diags) = compile_js_sources(&[("Main", "fun box(): Int = 1 + 2")]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(
            outputs,
            vec![(
                "Main.js".to_string(),
                b"function box() {\n  return (1 + 2);\n}\n".to_vec(),
            )]
        );
    }

    #[test]
    fn js_backend_passes_file_index_to_lowerer() {
        let (outputs, diags) = compile_js_sources(&[
            ("A", "fun first(): Int = 1"),
            ("B", "fun second(): Int = 2"),
        ]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(
            outputs,
            vec![
                (
                    "A.js".to_string(),
                    b"function first() {\n  return 1;\n}\n".to_vec(),
                ),
                (
                    "B.js".to_string(),
                    b"function second() {\n  return 2;\n}\n".to_vec(),
                ),
            ]
        );
    }

    #[test]
    fn js_backend_emits_statement_when_with_value_arm() {
        let (outputs, diags) = compile_js_sources(&[(
            "Main",
            "fun side(): Int = 1\n\
             fun box() {\n\
                 when {\n\
                     true -> {}\n\
                     else -> side()\n\
                 }\n\
             }",
        )]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(
            outputs,
            vec![(
                "Main.js".to_string(),
                b"function side() {\n  return 1;\n}\nfunction box() {\n  if (true) {\n    undefined;\n  }\n  else {\n    side();\n    undefined;\n  }\n}\n"
                    .to_vec(),
            )]
        );
    }

    #[test]
    fn js_backend_reports_unsupported_ir_lowering() {
        // A `tailrec` member function is rejected by common lowering before backend emission.
        let (outputs, diags) = compile_js_sources(&[(
            "Main",
            "class C { tailrec fun f(n: Int): Int = if (n == 0) 0 else f(n - 1) }\n\
             fun box(): Int = C().f(3)",
        )]);

        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostic_messages(&diags),
            vec!["krusty: this construct is not yet supported by the IR backend"]
        );
    }
}
