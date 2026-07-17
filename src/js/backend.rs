use crate::ast::File;
use crate::backend::{Artifact, Backend};
use crate::diag::DiagSink;
use crate::resolve::{SymbolTable, TypeInfo};
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
        file: &File,
        info: &TypeInfo,
        syms: &SymbolTable,
        stem: &str,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let Some(ir) = crate::ir_lower::lower_file(file, info, syms, &self.runtime) else {
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
    use crate::diag::DiagSink;
    use crate::frontend::{collect_signatures_with_cp, parse_source_with_detected_features};
    use crate::libraries::EmptySymbolSource;

    #[test]
    fn js_backend_runs_through_common_compiler_driver() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): Int = 1 + 2",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures_with_cp(&files, Box::new(EmptySymbolSource), &mut diags);
        let outputs = crate::compiler::compile(
            &files,
            &stems,
            &mut syms,
            &super::JsBackend::new(EmptySymbolSource),
            "main",
            &mut diags,
        );

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "Main.js");
        let js = std::str::from_utf8(&outputs[0].1).unwrap();
        assert!(js.contains("function box()"));
        assert!(js.contains("return (1 + 2);"));
    }

    #[test]
    fn js_backend_reports_unsupported_ir_lowering() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun f(vararg xs: Int): Int = 1\n\
             fun box(): Int { val a = intArrayOf(1, 2); return f(0, *a, 3) }",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures_with_cp(&files, Box::new(EmptySymbolSource), &mut diags);
        let outputs = crate::compiler::compile(
            &files,
            &stems,
            &mut syms,
            &super::JsBackend::new(EmptySymbolSource),
            "main",
            &mut diags,
        );

        assert!(outputs.is_empty());
        assert!(diags.has_errors());
        assert!(
            diags
                .diags
                .iter()
                .any(|d| d.msg.contains("not yet supported by the IR backend")),
            "{:?}",
            diags.diags
        );
    }
}
