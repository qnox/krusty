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

    fn lower_ir_file(
        &self,
        mut file: crate::backend::CheckedIrFile<'_>,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = &file.stems[file.source.raw() as usize];
        if let Err(target) = crate::backend::local_properties::realize(&mut file.ir) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot realize checked JS property access for {target:?}"),
            );
            return Vec::new();
        }
        super::control_flow::realize_updates(&mut file.ir);
        vec![(
            format!("{stem}.js"),
            super::emit_file(&file.ir).into_bytes(),
        )]
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::Artifact;
    use crate::diag::DiagSink;
    use crate::libraries::EmptySymbolSource;
    use crate::source::SourceInput;

    fn compile_js_sources(sources: &[(&str, &str)]) -> (Vec<Artifact>, DiagSink) {
        let mut diags = DiagSink::new();
        let inputs = sources
            .iter()
            .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
            .collect::<Vec<_>>();
        let stems = sources
            .iter()
            .map(|(stem, _)| (*stem).to_string())
            .collect::<Vec<_>>();
        let mut features = crate::features::LangFeatures::new();
        for (_, source) in sources {
            features.apply_source_directives(source);
        }
        let analysis = crate::frontend::analyze_source_set_streaming_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &features,
            &mut diags,
        );
        let outputs = crate::compiler::emit_analyzed(
            analysis,
            &stems,
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
                b"function side() {\n  return 1;\n}\nfunction box() {\n  if (true) {\n  }\n  else {\n    side();\n  }\n}\n"
                    .to_vec(),
            )]
        );
    }

    #[test]
    fn js_backend_emits_checked_block_expression() {
        let (outputs, diags) = compile_js_sources(&[(
            "Main",
            "class C { tailrec fun f(n: Int): Int = if (n == 0) 0 else f(n - 1) }\n\
             fun box(): Int = C().f(3)",
        )]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(outputs.len(), 1);
        let source = String::from_utf8(outputs[0].1.clone()).expect("JavaScript must be UTF-8");
        assert!(source.contains("return v2.f(v3);"), "{source}");
        assert!(!source.contains("cannot emit Block"), "{source}");
    }
}
