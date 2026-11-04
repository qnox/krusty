//! An `inline` function's `$default` stub contains the function body. A default lambda is a
//! class, not an `invokedynamic` bootstrap.

use super::common;

#[test]
fn an_inline_default_lambda_is_a_singleton_class() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultLambda",
        "inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultLambdaKt", "InlineDefaultLambdaKt$f$1"],
    );
}

#[test]
fn an_inline_default_lambda_captures_its_parameter() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultCapture",
        "inline fun f(x: Int, block: () -> Int = { x }): Int = block()\n",
        &["InlineDefaultCaptureKt", "InlineDefaultCaptureKt$f$1"],
    );
}

#[test]
fn a_same_file_caller_still_materializes_the_default_lambda() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "InlineDefaultCaller",
        "import java.io.OutputStream\n\
         import java.util.zip.ZipOutputStream\n\
         \n\
         internal inline fun zip(out: OutputStream, shuffle: (MutableList<String>) -> Unit = {}) {\n\
         \x20   out.use { outputStream ->\n\
         \x20       ZipOutputStream(outputStream).use { zipOutputStream ->\n\
         \x20           val paths = mutableListOf<String>()\n\
         \x20           shuffle(paths)\n\
         \x20           paths.forEach { path ->\n\
         \x20               zipOutputStream.write(path.length)\n\
         \x20           }\n\
         \x20       }\n\
         \x20   }\n\
         }\n\
         fun go(out: OutputStream) = zip(out)\n",
        // The caller's `use` expansion is a separate difference. This pins the default
        // lambda class, which a same-file caller copies into its body.
        &["InlineDefaultCallerKt$zip$1"],
    );
}

#[test]
fn a_private_inline_default_lambda_stays_out_of_the_public_abi() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultPrivate",
        "private inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultPrivateKt", "InlineDefaultPrivateKt$f$1"],
    );
}

#[test]
fn an_inline_default_lambda_can_own_another_lambda() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultNestedLambda",
        r#"inline fun test(
    block: () -> String = {
        var result = "O"
        val append = { result += "K" }
        append()
        result
    },
) = block()
"#,
        &[
            "InlineDefaultNestedLambdaKt",
            "InlineDefaultNestedLambdaKt$test$1",
            "InlineDefaultNestedLambdaKt$test$1$append$1",
        ],
    );
}

// kotlinc's `$default` stub of every inline function calls the function, and its inliner copies
// the body over that call. Whether the function has a default lambda does not matter.

const TOP_LEVEL: &str = r#"var seen = 0
inline fun f(x: Int = 1): Int = x + 1
private inline fun hidden(x: Int = 1): Int = x + 3
inline fun <reified T> isA(x: Any? = null): Boolean = x is T
suspend inline fun later(x: Int = 1): Int = x + 4
inline fun String.ext(x: Int = 1): Int = length + x
inline fun u(x: Int = 1) {
    seen += x
}
inline fun two(a: String = "a", b: String = a + "b"): String {
    val c = a + b
    return c
}
var log = ""
inline fun guarded(x: String = "a"): String {
    try {
        log += x
        return log
    } finally {
        log += "f"
    }
}
inline fun loop(n: Int = 3): Int {
    var total = 0
    for (i in 0 until n) {
        total += i
    }
    return total
}
"#;

#[test]
fn every_top_level_inline_default_stub_copies_the_body() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultBody",
        TOP_LEVEL,
        &["InlineDefaultBodyKt"],
    );
}

const MEMBERS: &str = r#"enum class E { A, B }
class C {
    inline fun g(x: Int = 1): Int = x + 2
    private inline fun hidden(x: Int = 1): Int = x + 2
    inline fun String.mext(x: Int = 1): Int = length + x
}
class Picker {
    inline fun pick(e: E, s: String = when (e) { E.A -> "OK"; E.B -> "FAIL" }): String = s
}
object O {
    inline fun h(x: Int = 1): Int = x + 5
    @JvmStatic inline fun hs(x: Int = 1): Int = x + 6
}
class K {
    companion object {
        inline fun c(x: Int = 1): Int = x + 7
        @JvmStatic inline fun cs(x: Int = 1): Int = x + 8
    }
}
open class Op {
    inline fun og(x: Int = 1): Int = x + 9
}
@JvmInline value class V(val v: Int) {
    inline fun plus(x: Int = 1): Int = v + x
}
"#;

#[test]
fn every_member_inline_default_stub_copies_the_body() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultMember",
        MEMBERS,
        &["C", "Picker", "O", "K", "K$Companion", "Op", "V"],
    );
}

#[test]
fn an_inline_default_stub_runs_its_copied_body() {
    let source = format!(
        "{TOP_LEVEL}{MEMBERS}{}",
        r#"inline fun caught(result: String = try { error("x") } catch (e: IllegalStateException) { "Y" }): String =
    result
fun useHidden(): Int = hidden()
class D {
    private inline fun hidden(x: Int = 1): Int = x + 2
    fun useHidden(): Int = hidden()
}
fun box(): String {
    u()
    val sum = f() + useHidden() + "ab".ext() + C().g() + D().useHidden() + with(C()) { "a".mext() } +
        O.h() + O.hs() + K.c() + K.cs() + Op().og() + V(1).plus()
    if (seen != 1 || isA<String>() || two() != "aab") return "FAIL: shapes"
    if (guarded() != "a" || log != "af" || loop() != 3 || caught() != "Y") return "FAIL: statements"
    if (Picker().pick(E.A) != "OK") return "FAIL: pick"
    return if (sum == 2 + 4 + 3 + 3 + 3 + 2 + 6 + 7 + 8 + 9 + 10 + 2) "OK" else "FAIL: $sum"
}
"#
    );
    assert_eq!(common::kotlinc_box_result(&source), "OK");
    common::expect_box_ok_with_stdlib(&source, "InlineDefaultBodyRun");
}

const SUSPEND: &str = r#"class Statement(val url: String) {
    suspend fun execute(): String = "ran " + url
}

suspend inline fun fetch(url: String, suffix: String = "/rate"): String {
    val target = url + suffix
    return Statement(target).execute()
}
"#;

#[test]
fn a_suspend_inline_default_stub_copies_the_body_its_inliners_take() {
    let classes = common::classes_against_kotlinc_module(&[("Fetch.kt", SUSPEND)]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
