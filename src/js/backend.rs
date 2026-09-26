use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;

#[derive(Default)]
pub struct JsBackend;

impl JsBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for JsBackend {
    type State = ();

    fn lower_ir_file(
        &self,
        mut file: crate::backend::CheckedIrFile<'_>,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = &file.stems[file.source.raw() as usize];
        crate::backend::counted_loops::realize(
            &mut file.ir,
            crate::backend::counted_loops::CounterLoopStyle::PreTested,
        );
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
            &super::JsBackend::new(),
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
            "class C {\n\
                 tailrec fun f(n: Int): Int = if (n == 0) 0 else f(n - 1)\n\
                 fun g(a: Int, b: Int): Int = a - b\n\
             }\n\
             fun box(): Int = C().g(b = C().f(3), a = 1)",
        )]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(outputs.len(), 1);
        let source = String::from_utf8(outputs[0].1.clone()).expect("JavaScript must be UTF-8");
        // What this test is about: a call whose receiver and arguments are spilled into
        // temporaries is a checked BLOCK expression, and emitting one as a VALUE needs the
        // block-expression path rather than the statement one. `box()`'s named arguments, written
        // out of order, are that shape.
        assert!(source.contains("return v0.g(v2, v1);"), "{source}");
        assert!(!source.contains("cannot emit Block"), "{source}");
        // A member `tailrec` whose self-call dispatches on `this` is the same frame and steps
        // (docs/SPEC.md, "What `tailrec` loops is a FRAME"). The step sits in a branch of the
        // returned `if`, so the `return` moves into the branches and the `continue` stays a
        // statement of the loop. Asserted here because this backend renders the rewrite's output
        // directly, so it is the cheapest place to see that the rewrite reaches every backend.
        assert!(source.contains("continue $tailrec;"), "{source}");
    }

    /// `a ?: b ?: f(x - 1)` coerces the inner elvis and coerces that again, so the loop step sits
    /// under a coercion around a `when`. The `return` moves through the coercion into the arms, so
    /// the `continue` is a statement of the loop and never an expression.
    #[test]
    fn js_backend_moves_a_return_through_an_elvis_coercion() {
        let (outputs, diags) = compile_js_sources(&[(
            "Main",
            "tailrec fun chained(x: Int): Int? {\n\
                 if (x < 0) return null\n\
                 if (x == 0) return 7\n\
                 return chained(-1) ?: chained(-2) ?: chained(x - 1)\n\
             }",
        )]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        let source = String::from_utf8(outputs[0].1.clone()).expect("JavaScript must be UTF-8");
        assert_eq!(
            source,
            "function chained(v0) {\n  $tailrec:\n  do {\n    if ((v0 < 0)) {\n      return null;\n    }\n    else {\n    }\n    if ((v0 === 0)) {\n      return 7;\n    }\n    else {\n    }\n    let v1 = chained(-1);\n    if ((v1 === null)) {\n      let v2 = chained(-2);\n      if ((v2 === null)) {\n        let v3 = (v0 - 1);\n        v0 = v3;\n        continue $tailrec;\n      }\n      else {\n        return v2;\n      }\n    }\n    else {\n      return v1;\n    }\n    break $tailrec;\n  } while (true);\n}\n"
        );
    }

    #[test]
    fn js_backend_realizes_checked_range_loop_with_its_own_shape() {
        let (outputs, diags) = compile_js_sources(&[(
            "Main",
            "fun sum(n: Int): Int { var total = 0; for (item in 0..<n) total += item; return total }",
        )]);

        assert_eq!(diagnostic_messages(&diags), Vec::<&str>::new());
        assert_eq!(
            outputs,
            vec![(
                "Main.js".to_string(),
                b"function sum(v0) {\n  let v1 = 0;\n  let v2 = 0;\n  $fir_control_0_1:\n  while ((v2 < v0)) {\n    v1 = (v1 + v2);\n    v2 = (v2 + 1);\n  }\n  return v1;\n}\n"
                    .to_vec(),
            )]
        );
    }
}
