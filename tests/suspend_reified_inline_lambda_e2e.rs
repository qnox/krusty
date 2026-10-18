//! A suspension in the lambda of a reified classpath inline function goes through kotlinc's
//! coroutine transformer: the method inliner places the lambda, which captures the continuation
//! the enclosing function passes on, and the transformer builds the machine over the inlined body.
//! The byte splice never specializes a reified body, so this shape had no route before.

use super::common;

const SOURCE: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
import kotlin.test.assertFailsWith\n\
\n\
suspend fun fail(): Int = throw UnsupportedOperationException(\"boom\")\n\
\n\
suspend fun named(): String =\n\
    assertFailsWith<UnsupportedOperationException>(\"named\") { fail() }.message!!\n\
\n\
fun launch(block: suspend () -> Unit) {\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
\n\
fun box(): String {\n\
    var result = \"\"\n\
    launch { result += named() }\n\
    launch {\n\
        val message = assertFailsWith<UnsupportedOperationException> { fail() }.message\n\
        result += message\n\
    }\n\
    return if (result == \"boomboom\") \"OK\" else result\n\
}\n";

fn classpath() -> Vec<std::path::PathBuf> {
    let kotlin_test = common::kotlin_test_jar()
        .expect("no kotlin-test jar found; provision the reference toolchain (`just` fetches it)");
    vec![common::stdlib_jar(), kotlin_test]
}

/// A named suspend function and two suspend lambdas whose only suspension sits in an
/// `assertFailsWith` lambda: every class is the reference compiler's, byte for byte.
#[test]
fn a_suspension_in_a_reified_inline_lambda_matches_kotlinc() {
    for class in [
        "ReifiedInlineLambdaKt",
        "ReifiedInlineLambdaKt$named$1",
        "ReifiedInlineLambdaKt$box$1",
        "ReifiedInlineLambdaKt$box$2",
    ] {
        let compared = common::compare_with_kotlinc_plugin(
            "ReifiedInlineLambda",
            SOURCE,
            class,
            &classpath(),
            "25",
            &[],
        )
        .unwrap_or_else(|| panic!("reference kotlinc and javap must be available"));
        assert!(
            compared.krusty_bytes == compared.reference_bytes,
            "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            compared.reference,
            compared.krusty
        );
    }
}

/// The machines resume through the inlined lambdas' handlers: each caught failure is the one the
/// suspending call threw.
#[test]
fn a_suspension_in_a_reified_inline_lambda_runs() {
    let mut classpath = classpath();
    let jdk = common::jdk_modules();
    classpath.push(jdk.clone());
    assert_eq!(
        common::expect_box_run(
            SOURCE,
            "ReifiedInlineLambda",
            &classpath,
            Some(jdk.as_path())
        ),
        "OK"
    );
}
