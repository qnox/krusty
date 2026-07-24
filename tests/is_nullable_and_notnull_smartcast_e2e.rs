//! `x is Int? && x != null` proves `x` is a non-null `Int` — the `is Int?` leaf narrows `x` to the
//! nullable-primitive wrapper, and the `!= null` leaf then strips the `?`. kotlinc smart-casts the
//! conjunction; krusty's `&&`-chain collector treated each leaf independently, so the `!= null`
//! refinement was lost on a receiver whose declared type (`Any?`) has no nullable-primitive form,
//! and a later use of `x` at an `Int` parameter failed with "inferred type is Int but Int was
//! expected". Round-tripped on the JVM.

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
fn is_nullable_primitive_and_notnull_narrows_to_the_primitive() {
    // The boxing4.kt shape: `arg is Int? && arg != null` lets `arg` flow to an `Int` parameter.
    const SRC: &str = "var got = \"\"\n\
fun printInt(x: Int) { got += \"$x;\" }\n\
fun foo(arg: Any?) {\n\
    if (arg is Int? && arg != null) printInt(arg)\n\
}\n\
fun box(): String {\n\
    foo(16)\n\
    foo(null)\n\
    foo(\"skip\")\n\
    return if (got == \"16;\") \"OK\" else \"FAIL: $got\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("is Int? && != null smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn notnull_and_is_nullable_primitive_narrows_in_either_order() {
    const SRC: &str = "var got = \"\"\n\
fun printInt(x: Int) { got += \"$x;\" }\n\
fun foo(arg: Any?) {\n\
    if (arg != null && arg is Int?) printInt(arg)\n\
}\n\
fun box(): String {\n\
    foo(7)\n\
    foo(null)\n\
    return if (got == \"7;\") \"OK\" else \"FAIL: $got\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("!= null && is Int? smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn is_nullable_primitive_alone_does_not_reach_a_nonnull_param() {
    // Without the `!= null` leaf the value stays `Int?` — kotlinc rejects it at an `Int` parameter.
    const SRC: &str = "fun printInt(x: Int) {}\n\
fun foo(arg: Any?) {\n\
    if (arg is Int?) printInt(arg)\n\
}\n\
fun box(): String { foo(1); return \"OK\" }\n";
    assert_rejected(SRC);
}

#[test]
fn is_nullable_unsigned_and_notnull_does_not_narrow() {
    // Unsigned stays unnarrowed (its value-box unbox to `kotlin.UInt` isn't modeled) — the chain
    // refinement skips it and the `take(arg)` use still skips the file, same as the `is UInt` policy.
    const SRC: &str = "fun take(u: UInt) = u.toString()\n\
fun foo(arg: Any?): String {\n\
    if (arg is UInt? && arg != null) return take(arg)\n\
    return \"no\"\n\
}\n\
fun box(): String = if (foo(5u) == \"5\" && foo(null) == \"no\") \"OK\" else \"FAIL\"\n";
    assert_rejected(SRC);
}

#[test]
fn corpus_boxing4_is_nullable_and_notnull() {
    if let Some(out) = common::run_box_corpus_case("boxing/boxing4.kt") {
        assert_eq!(out, "OK");
    }
}
