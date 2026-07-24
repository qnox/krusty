//! `x ?: return` (or any elvis whose right-hand side never completes — `throw`, `break`,
//! `continue`, a `Nothing`-typed call) proves `x` non-null for the rest of the straight-line
//! block, exactly like an `if (x == null) return` guard. kotlinc smart-casts the later reads;
//! krusty only narrowed on the `if`-guard form, so a later use of `x` at a non-null parameter
//! failed with "type mismatch: inferred type is Int but Int was expected" (KT-9277 shape).
//! Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_rejected(src: &str) {
    assert!(
        common::compile_and_run_with_stdlib(src, "Main").is_none(),
        "source should be rejected, but compiled successfully:\n{src}"
    );
}

#[test]
fn elvis_return_smartcasts_primitive_param_for_later_use() {
    // The KT-9277 shape: after `z = x ?: return`, the named argument `y = x` reads `x` as `Int`.
    const SRC: &str = "var got = \"\"\n\
fun bar(y: Int, z: Int) { got = \"$y/$z\" }\n\
fun foo(x: Int?) {\n\
    bar(z = x ?: return, y = x)\n\
}\n\
fun box(): String {\n\
    foo(null)\n\
    if (got != \"\") return \"FAIL null: $got\"\n\
    foo(7)\n\
    return if (got == \"7/7\") \"OK\" else \"FAIL: $got\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("elvis-return smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn elvis_return_smartcasts_local_val_in_following_statements() {
    const SRC: &str = "fun add(a: Int, b: Int) = a + b\n\
fun foo(x: Int?): Int {\n\
    val z = x ?: return -1\n\
    return add(x, z)\n\
}\n\
fun box(): String {\n\
    if (foo(null) != -1) return \"FAIL null\"\n\
    return if (foo(21) == 42) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("elvis-return val smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn elvis_throw_smartcasts_reference_param() {
    const SRC: &str = "fun len(s: String) = s.length\n\
fun foo(s: String?): Int {\n\
    s ?: throw IllegalStateException(\"null\")\n\
    return len(s)\n\
}\n\
fun box(): String = if (foo(\"hello\") == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("elvis-throw smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn elvis_nothing_call_smartcasts_for_later_use() {
    const SRC: &str = "fun twice(n: Int) = n * 2\n\
fun foo(x: Int?): Int {\n\
    val z = x ?: error(\"null\")\n\
    return twice(x) + z\n\
}\n\
fun box(): String = if (foo(14) == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("elvis-error() smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn non_diverging_elvis_does_not_narrow() {
    // `x ?: 0` proves nothing about `x` afterwards — the later `y = x` must stay `Int?` and be
    // rejected at the `Int` parameter.
    const SRC: &str = "fun bar(y: Int, z: Int) {}\n\
fun foo(x: Int?) {\n\
    bar(z = x ?: 0, y = x)\n\
}\n\
fun box(): String { foo(null); return \"OK\" }\n";
    assert_rejected(SRC);
}

#[test]
fn closure_reassigned_var_is_not_narrowed() {
    // A `var` written inside a closure can be reset to null on a deferred path — kotlinc refuses
    // the smart-cast, and so must krusty.
    const SRC: &str = "fun bar(y: Int) {}\n\
fun foo() {\n\
    var x: Int? = 5\n\
    val reset = { x = null }\n\
    x ?: return\n\
    reset()\n\
    bar(y = x)\n\
}\n\
fun box(): String { foo(); return \"OK\" }\n";
    assert_rejected(SRC);
}

#[test]
fn elvis_return_does_not_narrow_unsigned() {
    // Unsigned stays unnarrowed (its value-box unbox to `kotlin.UInt` isn't modeled) — the later
    // `take(x)` still sees `UInt?` and the file skips, same as the `is UInt` smart-cast policy.
    const SRC: &str = "fun take(u: UInt) = u.toString()\n\
fun foo(x: UInt?): String {\n\
    val z = x ?: return \"null\"\n\
    return take(x) + z.toString()\n\
}\n\
fun box(): String = if (foo(3u) == \"33\" && foo(null) == \"null\") \"OK\" else \"FAIL\"\n";
    assert_rejected(SRC);
}

#[test]
fn corpus_kt9277_named_argument_elvis_return() {
    if let Some(out) = common::run_box_corpus_case("argumentOrder/kt9277.kt") {
        assert_eq!(out, "OK");
    }
}
