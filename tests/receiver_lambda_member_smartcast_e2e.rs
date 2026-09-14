//! A member `val`'s null proof must survive a lambda that introduces a DIFFERENT receiver.
//!
//! A bare own-member read (`token`) is an alias for a dispatch-property read, so the stability
//! decision is made against the receiver that owns the property. krusty made it against whatever
//! `this` meant at the proof site instead. Inside `outer { … }` — a lambda typed
//! `Outer.() -> String` — `this` is `Outer`, which has no such member, so the proof was silently
//! dropped: `if (token != null) need(token)` still saw `String?` and the file was rejected. The
//! owning receiver is still on the scope's rung stack; only the innermost one had moved.

use super::common;

fn assert_both_accept(source: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc must accept the exact receiver-lambda fixture",
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the same source without diagnostics",
    );
}

fn assert_exact_rejection(
    source: &str,
    line: usize,
    source_line: &str,
    reference_message: &str,
    krusty_message: &str,
) {
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &[]);
    assert_eq!(result.krusty_stdout, "", "unexpected krusty stdout");
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc diagnostic path");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:{line}:60: error: {reference_message}\n{source_line}\n{}^^^^^\n",
                " ".repeat(59),
            )
            .as_str(),
        ),
        "exact kotlinc diagnostic",
    );
    let krusty_path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty diagnostic path");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!("{krusty_path}:{line}:60: error: {krusty_message}\nkrusty: 1 error(s)\n")
                .as_str(),
        ),
        "exact krusty diagnostic",
    );
}

/// The failing shape: the member is proven non-null inside a receiver lambda of an unrelated type.
#[test]
fn a_member_val_smart_casts_inside_a_foreign_receiver_lambda() {
    const MAIN: &str = "class Outer\n\
fun need(value: String): String = value\n\
class Holder(private val token: String?) {\n\
\x20   fun outer(block: Outer.() -> String): String = Outer().block()\n\
\x20   fun render(): String = outer { if (token != null) need(token) else \"none\" }\n\
}\n\
fun box(): String {\n\
\x20   if (Holder(\"ok\").render() != \"ok\") return \"FAIL: present\"\n\
\x20   if (Holder(null).render() != \"none\") return \"FAIL: absent\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_both_accept(MAIN);
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "receiver_lambda_member_smartcast",
    );
}

/// Nesting more foreign receivers between the proof and the read changes nothing — the owning rung
/// is still the one the binding names.
#[test]
fn the_proof_survives_several_nested_foreign_receivers() {
    const MAIN: &str = "class Outer\n\
class Middle\n\
fun need(value: String): String = value\n\
fun Outer.middle(block: Middle.() -> String): String = Middle().block()\n\
class Holder(private val token: String?) {\n\
\x20   fun outer(block: Outer.() -> String): String = Outer().block()\n\
\x20   fun render(): String = outer { if (token != null) middle { need(token) } else \"none\" }\n\
}\n\
fun box(): String {\n\
\x20   if (Holder(\"ok\").render() != \"ok\") return \"FAIL: present\"\n\
\x20   if (Holder(null).render() != \"none\") return \"FAIL: absent\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_both_accept(MAIN);
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "nested_foreign_receiver_smartcast",
    );
}

/// An unstable member must stay unstable on this path: a `var` re-reads its accessor, so kotlinc
/// reports SMARTCAST_IMPOSSIBLE and krusty must reject the same file with its own message. Both
/// compilers' complete output is asserted so widening the proof cannot silently open a hole.
#[test]
fn a_mutable_member_still_declines_inside_a_foreign_receiver_lambda() {
    const MAIN: &str = "class Outer\n\
fun need(value: String): String = value\n\
class Holder(private var token: String?) {\n\
\x20   fun outer(block: Outer.() -> String): String = Outer().block()\n\
\x20   fun render(): String = outer { if (token != null) need(token) else \"none\" }\n\
}\n";
    assert_exact_rejection(
        MAIN,
        5,
        "    fun render(): String = outer { if (token != null) need(token) else \"none\" }",
        "smart cast to 'String' is impossible, because 'token' is a mutable property that could be mutated concurrently.",
        "argument type mismatch: actual type is 'String?', but 'String' was expected.",
    );
}

/// A custom getter is re-entered on every read, so it must decline here exactly as it does for the
/// qualified `this.token` spelling.
#[test]
fn a_custom_getter_still_declines_inside_a_foreign_receiver_lambda() {
    const MAIN: &str = "class Outer\n\
fun need(value: String): String = value\n\
class Holder(private val raw: String?) {\n\
\x20   private val token: String? get() = raw\n\
\x20   fun outer(block: Outer.() -> String): String = Outer().block()\n\
\x20   fun render(): String = outer { if (token != null) need(token) else \"none\" }\n\
}\n";
    assert_exact_rejection(
        MAIN,
        6,
        "    fun render(): String = outer { if (token != null) need(token) else \"none\" }",
        "smart cast to 'String' is impossible, because 'token' is a property that has an open or custom getter.",
        "argument type mismatch: actual type is 'String?', but 'String' was expected.",
    );
}
