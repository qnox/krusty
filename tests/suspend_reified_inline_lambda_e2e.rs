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

/// A member's suspend lambda reaches its class's state through the captured outer instance, which
/// the inlined lambda captures in turn: kotlinc evaluates that field read into a temporary.
const MEMBER_SOURCE: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
import kotlin.test.assertFailsWith\n\
\n\
suspend fun off(id: String): Int = throw UnsupportedOperationException(id)\n\
\n\
fun launch(block: suspend () -> Unit) {\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
\n\
class Holder {\n\
    private val word = \"OK\"\n\
    var seen = \"\"\n\
    fun run() = launch {\n\
        val message = assertFailsWith<UnsupportedOperationException> { off(word) }.message!!\n\
        seen = message\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val holder = Holder()\n\
    holder.run()\n\
    return holder.seen\n\
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

/// The member's lambda class reads the outer instance into a temporary before the inlined body.
#[test]
fn a_reified_inline_lambda_capturing_the_outer_instance_matches_kotlinc() {
    for class in ["Holder", "Holder$run$1"] {
        let compared = common::compare_with_kotlinc_plugin(
            "ReifiedMemberLambda",
            MEMBER_SOURCE,
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

/// The inlined lambda reads the member's state through the outer instance it captured.
#[test]
fn a_reified_inline_lambda_capturing_the_outer_instance_runs() {
    let mut classpath = classpath();
    let jdk = common::jdk_modules();
    classpath.push(jdk.clone());
    assert_eq!(
        common::expect_box_run(
            MEMBER_SOURCE,
            "ReifiedMemberLambda",
            &classpath,
            Some(jdk.as_path())
        ),
        "OK"
    );
}

/// A reified library function whose lambda sits between two ordinary arguments.
const BETWEEN_LIB: &str = "package lib\n\
\n\
inline fun <reified T> around(before: String, block: () -> Unit, after: String): String {\n\
    \x20   block()\n\
    \x20   return before + T::class.java.simpleName + after\n\
}\n";

const BETWEEN_MAIN: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
\n\
suspend fun record(text: String, into: StringBuilder) {\n\
    \x20   into.append(text)\n\
}\n\
\n\
fun launch(block: suspend () -> Unit) {\n\
    \x20   block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
\n\
fun left(): String = \"<\"\n\
\n\
fun right(): String = \">\"\n\
\n\
class Holder {\n\
    \x20   private val id = \"O\"\n\
    \x20   val log = StringBuilder()\n\
\n\
    \x20   fun run() = launch {\n\
    \x20       val shown = lib.around<String>(left(), { record(id, log) }, right())\n\
    \x20       log.append(shown)\n\
    \x20   }\n\
}\n\
\n\
fun box(): String {\n\
    \x20   val holder = Holder()\n\
    \x20   holder.run()\n\
    \x20   return if (holder.log.toString() == \"O<String>\") \"OK\" else holder.log.toString()\n\
}\n";

/// The outer instance a lambda between two arguments captures is stored where the lambda stands:
/// its temporary follows the argument before it and precedes the one after it, as kotlinc's.
#[test]
fn a_captured_outer_instance_between_arguments_is_stored_where_its_lambda_stands() {
    let library = common::compile_lib("reified_lambda_between_arguments", BETWEEN_LIB)
        .expect("krusty builds the library");
    let jdk = common::jdk_modules();
    let classpath = [library, common::stdlib_jar()];
    assert_eq!(
        common::compile_and_run_box(BETWEEN_MAIN, "Main", &classpath, Some(jdk.as_path()))
            .as_deref(),
        Some("OK")
    );
    for class in ["Holder", "Holder$run$1"] {
        let compared =
            common::compare_with_kotlinc_plugin("Main", BETWEEN_MAIN, class, &classpath, "25", &[])
                .expect("reference kotlinc and javap must be available");
        assert!(
            compared.krusty_bytes == compared.reference_bytes,
            "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            compared.reference,
            compared.krusty
        );
    }
}
