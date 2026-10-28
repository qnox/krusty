//! Smart casts established by an elvis expression with a diverging right-hand side.

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
    const SRC: &str = "fun bar(y: Int) {}\n\
fun foo() {\n\
    var x: Int? = 5\n\
    val reset = { x = null }\n\
    x ?: return\n\
    reset()\n\
    bar(y = x)\n\
}\n\
fun box(): String { foo(); return \"OK\" }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics,
        ["smart cast to 'Int' is impossible, because 'x' is a local variable that is mutated in a capturing closure."]
    );
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

#[test]
fn a_succeeded_safe_cast_narrows_its_operand() {
    // `x as? T` is non-null only when the cast succeeded, so a diverging elvis or a `!= null`
    // check on it proves `x` is `T`, for a parameter and for a stable property path alike.
    const SRC: &str = "class Box<T>(val value: T)\n\
@Suppress(\"UNCHECKED_CAST\")\n\
fun elvisError(x: Any?): String {\n\
    (x as? Box<String>) ?: error(\"not a box\")\n\
    return x.value\n\
}\n\
fun elvisReturn(x: Any?): String {\n\
    x as? String ?: return \"none\"\n\
    return x\n\
}\n\
fun condition(x: Any?): String = if (x as? String != null) x else \"none\"\n\
class Holder(val item: Any?)\n\
fun property(holder: Holder): String {\n\
    holder.item as? String ?: return \"none\"\n\
    return holder.item\n\
}\n\
fun box(): String {\n\
    if (elvisReturn(1) != \"none\" || condition(2) != \"none\" || property(Holder(3)) != \"none\") return \"FAIL\"\n\
    return elvisError(Box(\"O\")) + elvisReturn(\"K\") + condition(\"\") + property(Holder(\"\"))\n\
}\n";
    assert_eq!(
        common::reference_error_messages("Main", SRC),
        Vec::<String>::new(),
        "kotlinc must accept the safe-cast smart casts"
    );
    assert_eq!(run(SRC).expect("safe-cast smart casts compile + run"), "OK");
}

#[test]
fn a_safe_cast_proves_nothing_without_divergence_or_stability() {
    // A non-diverging elvis leaves `x` unnarrowed, and a closure-mutated `var` declines the cast
    // with kotlinc's smart-cast diagnostic.
    const SRC: &str = "fun fallback(x: Any?): String {\n\
    x as? String ?: \"fallback\"\n\
    return x\n\
}\n\
fun mutated(): String {\n\
    var x: Any? = \"a\"\n\
    val reset = { x = 1 }\n\
    x as? String ?: return \"none\"\n\
    reset()\n\
    return x\n\
}\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:3:8: return type mismatch: expected 'String', actual 'Any?'.",
            "Main.kt:10:8: smart cast to 'String' is impossible, because 'x' is a local variable that is mutated in a capturing closure.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_non_null_safe_cast_to_a_nullable_target_narrows_to_its_non_null_form() {
    // The proof is of a non-null result, so `as? String?` narrows `x` to `String`, not `String?`.
    const SRC: &str = "fun nullableTarget(x: Any?): Int {\n\
    (x as? String?) ?: return 0\n\
    return x.length\n\
}\n\
fun box(): String =\n\
    if (nullableTarget(\"ab\") == 2 && nullableTarget(null) == 0 && nullableTarget(1) == 0) \"OK\" else \"fail\"\n";
    assert_eq!(
        common::reference_error_messages("Main", SRC),
        Vec::<String>::new(),
        "kotlinc must narrow through a nullable safe-cast target"
    );
    assert_eq!(
        run(SRC).expect("nullable-target smart cast compiles + runs"),
        "OK"
    );
}

#[test]
fn a_nullable_safe_cast_target_proves_only_its_non_null_form() {
    // `as? Any?` proves only `Any`, and a closure-mutated `var` declines the non-null target.
    const SRC: &str = "fun anyTarget(x: Any?): Int {\n\
    (x as? Any?) ?: return 0\n\
    return x.length\n\
}\n\
fun mutatedNullableTarget(): Int {\n\
    var x: Any? = \"a\"\n\
    val reset = { x = 1 }\n\
    (x as? String?) ?: return 0\n\
    reset()\n\
    return x.length\n\
}\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:3:10: unresolved reference 'length' on receiver of type 'Any'.",
            "Main.kt:10:8: smart cast to 'String' is impossible, because 'x' is a local variable that is mutated in a capturing closure.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}
