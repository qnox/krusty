//! Fully-qualified static calls `pkg.Type.method(args)` — the receiver is a DOTTED chain that names a
//! classpath type verbatim (`java.time.Instant.parse(it)`), not an imported simple name. krusty resolves
//! the chain to a type and dispatches the static member. Also exercises subtype-widening static overload
//! resolution: a Java static exposing only a SUPERTYPE-parameter overload (`Instant.parse(CharSequence)`)
//! is selected for a `String` argument. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn fully_qualified_static_method_calls() {
    // `java.lang.Integer.parseInt` (exact match), `java.lang.Math.max` (exact), and
    // `java.lang.String.valueOf(Int)` (exact primitive overload) — all via the dotted receiver chain.
    const SRC: &str = "fun box(): String {\n\
    if (java.lang.Integer.parseInt(\"42\") != 42) return \"Fail 1\"\n\
    if (java.lang.Math.max(3, 7) != 7) return \"Fail 2\"\n\
    if (java.lang.String.valueOf(5) != \"5\") return \"Fail 3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("fq static calls"), "OK");
}

#[test]
fn fully_qualified_static_call_with_subtype_widening() {
    // `java.time.Instant.parse(CharSequence)` selected for a `String` argument — the exact descriptor is
    // `CharSequence`, so resolution must widen `String` <: `CharSequence`. Nested as an argument to a
    // second FQN-typed static (`java.util.Date.from(Instant)`).
    const SRC: &str = "fun box(): String {\n\
    val i = java.time.Instant.parse(\"2020-01-01T00:00:00Z\")\n\
    val d = java.util.Date.from(i)\n\
    if (d.toInstant() != i) return \"Fail 1\"\n\
    if (i.getEpochSecond() != 1577836800L) return \"Fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("fq static call widening"), "OK");
}
