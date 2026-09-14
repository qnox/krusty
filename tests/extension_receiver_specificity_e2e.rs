//! Declaration specificity includes an extension's resolved receiver identity alongside its
//! source-argument-aligned value-parameter shapes.

use super::common;

fn assert_both_run(src: &str, stem: &str) {
    let result = common::compiler_diagnostics(&[("Main.kt", src)], &[]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the extension-specificity fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty rejected the extension-specificity fixture"
    );
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, stem);
}

#[test]
fn a_plain_overload_wins_an_equal_receiver_tiebreaker() {
    const SRC: &str = "class Ctx\n\
class Node\n\
fun Node.act(path: String, body: Ctx.() -> Unit): String = \"plain\"\n\
@JvmName(\"actTypedPath\")\n\
inline fun <reified R : Any> Node.act(\n\
    path: String,\n\
    noinline body: Ctx.(R) -> Unit,\n\
): String = \"typed\"\n\
fun box(): String = if (Node().act(\"/a\") {} == \"plain\") \"OK\" else \"typed\"\n";
    assert_both_run(SRC, "ExtensionSpecificityTiebreak");
}

#[test]
fn a_bounded_receiver_survives_named_default_and_spread_mapping() {
    const SRC: &str = "interface Named\n\
class Good : Comparable<Good>, Named {\n\
    override fun compareTo(other: Good): Int = 0\n\
}\n\
class Bad : Comparable<Bad> {\n\
    override fun compareTo(other: Bad): Int = 0\n\
}\n\
fun <T> T.pick(prefix: String = \"\", vararg values: Any): String\n\
    where T : Comparable<T>, T : Named = \"bounded\"\n\
fun Any.pick(prefix: String = \"\", vararg values: Any): String = \"fallback\"\n\
fun box(): String {\n\
    val values = arrayOf(\"x\")\n\
    if (Good().pick() != \"bounded\") return \"default\"\n\
    if (Good().pick(values = values) != \"bounded\") return \"named\"\n\
    if (Good().pick(\"p\", *values) != \"bounded\") return \"spread\"\n\
    if (Bad().pick(values = values) != \"fallback\") return \"bad\"\n\
    return \"OK\"\n\
}\n";
    assert_both_run(SRC, "ExtensionSpecificityReceiver");
}
