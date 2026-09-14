//! A member `val`'s null proof must survive a lambda that introduces a DIFFERENT receiver.
//!
//! A bare own-member read (`token`) is an alias for a dispatch-property read, so the stability
//! decision is made against the receiver that owns the property. krusty made it against whatever
//! `this` meant at the proof site instead. Inside `outer { … }` — a lambda typed
//! `Outer.() -> String` — `this` is `Outer`, which has no such member, so the proof was silently
//! dropped: `if (token != null) need(token)` still saw `String?` and the file was rejected. The
//! owning receiver is still on the scope's rung stack; only the innermost one had moved.

use super::common;

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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "receiver_lambda_member_smartcast");
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "nested_foreign_receiver_smartcast");
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
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a `var` smart cast: {}",
        result.reference_stderr
    );
    assert!(
        result
            .reference_stderr
            .contains("smart cast to 'String' is impossible, because 'token' is a mutable property"),
        "unexpected kotlinc output: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a `var` smart cast kotlinc rejects: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    assert!(
        result
            .krusty_stderr
            .contains("argument type mismatch: actual type is 'String?', but 'String' was expected."),
        "unexpected krusty output: {}{}",
        result.krusty_stdout,
        result.krusty_stderr
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
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a custom-getter smart cast: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a custom-getter smart cast kotlinc rejects: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
