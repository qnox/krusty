//! A literal lambda spliced into `Closeable.use` stores its result in the parameter's slot and
//! reloads that slot after `finally`.

use super::common;

const SOURCE: &str = r#"import java.io.Closeable

fun pack(dir: String, out: Closeable) {
    val expanded = dir
    out.use {
        println(expanded)
        println(dir)
    }
}

fun value(dir: String, out: Closeable): String {
    val expanded = dir
    return out.use { expanded + dir }
}

fun effect(dir: String, out: Closeable) {
    val expanded = dir
    out.use { println(expanded) }
}

fun retUnit(out: Closeable): Unit = out.use { println("x") }

fun retString(dir: String, out: Closeable): String = out.use { dir }

fun identity(out: Closeable): Closeable = out.use { it }
"#;

#[test]
fn a_literal_use_lambda_reloads_its_result_after_finally() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "InlineUseLambda",
        SOURCE,
        &["InlineUseLambdaKt"],
    );
}

/// The store/reload/landing sequence above is a JVM representation choice. Common lowering keeps
/// each protected lambda value as a result-bearing `try` and records its inline-cleanup provenance;
/// it must not manufacture a source-absent loop to prescribe that bytecode shape.
#[test]
fn an_inline_cleanup_result_stays_a_semantic_try_in_common_ir() {
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (files, diagnostics) = common::capture_common_ir(SOURCE, "InlineUseLambda", platform);
    assert_eq!(diagnostics, Vec::<String>::new());
    let file = files.into_iter().next().expect("one lowered source file");
    assert_eq!(file.inline_cleanup_results.len(), 6);
    assert_eq!(
        file.exprs
            .iter()
            .filter(|expression| matches!(expression, krusty::ir::IrExpr::While { .. }))
            .count(),
        0,
        "JVM result transport must not become common control flow"
    );
    for expression in &file.inline_cleanup_results {
        assert!(
            matches!(
                file.expr(*expression),
                krusty::ir::IrExpr::Try {
                    finally: Some(_),
                    ..
                }
            ),
            "inline cleanup provenance must point at its semantic try"
        );
    }
}
