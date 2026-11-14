//! Complete compiler rejection reports shared by corpus and in-process e2e compilation.

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::diag::{DiagSink, Span};
    use krusty::source::SourceKind;

    fn header(module: &str) -> String {
        format!(
            "compiler diagnostics [{module}] test={}:\n",
            std::thread::current().name().unwrap_or("unnamed worker")
        )
    }

    #[test]
    fn rejection_keeps_every_diagnostic_in_order_with_its_own_source_kind() {
        let mut diagnostics = DiagSink::new();
        diagnostics.set_file(2);
        diagnostics.error(Span::new(3, 4), "third-file error");
        diagnostics.set_file(0);
        diagnostics.warning(Span::new(1, 2), "first-file warning");
        diagnostics.set_file(1);
        diagnostics.error(Span::new(0, 1), "second-file error");
        let report = render_rejection(
            "sample",
            [
                (SourceKind::Java, "Helper", "class H {}"),
                (SourceKind::Kotlin, "Main.kt", "fun f() {}"),
                (SourceKind::KotlinScript, "script", "ab\ncd"),
            ],
            &diagnostics,
        );
        assert_eq!(
            report,
            Some(format!(
                "{}script.kts:2:1: error: third-file error\nHelper.java:1:2: warning: first-file warning\nMain.kt:1:1: error: second-file error\n",
                header("sample")
            ))
        );
    }

    #[test]
    fn warnings_do_not_become_compiler_rejections() {
        let mut diagnostics = DiagSink::new();
        diagnostics.warning(Span::new(0, 1), "a warning");
        assert_eq!(
            render_rejection("main", [(SourceKind::Kotlin, "Main", "x")], &diagnostics),
            None
        );
    }

    #[test]
    fn an_unlocated_module_error_is_not_lost() {
        let mut diagnostics = DiagSink::new();
        diagnostics.module_error("the selected declaration has no annotation facts");
        assert_eq!(
            render_rejection("main", [], &diagnostics),
            Some(format!(
                "{}error: the selected declaration has no annotation facts\n",
                header("main")
            ))
        );
    }
}
