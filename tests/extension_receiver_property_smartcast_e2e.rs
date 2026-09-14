//! A bare property of an EXTENSION receiver smart-casts.
//!
//! `fun Yaml.expand() = if (ref != null) take(ref) else …` reads `ref` through the extension
//! receiver, so the proof `ref != null` is a fact about `this.ref`. The narrowing was FILED under the
//! bare name `ref` — the spelling in the source — while the later read resolves through the receiver
//! and asks for `this.ref`. Recorder and lookup disagreed on the key, the proof was never found, and
//! the argument was still `String?`:
//!
//! ```text
//! error: argument type mismatch: actual type is 'String?', but 'String' was expected.
//! ```
//!
//! A MEMBER function was unaffected: its properties are bound in the lexical value namespace, which
//! takes the shadowing path instead and never consults the property-path key at all.

use super::common;

/// The failing shape, in both the expression-body and block-body spellings.
#[test]
fn an_extension_receivers_property_smart_casts_under_a_bare_name() {
    const MAIN: &str = "class Yaml(val ref: String?)\n\
fun take(value: String): String = value\n\
fun Yaml.inline(): String = if (ref != null) take(ref) else \"none\"\n\
fun Yaml.block(): String {\n\
\x20   if (ref != null) {\n\
\x20       return take(ref)\n\
\x20   }\n\
\x20   return \"none\"\n\
}\n\
fun box(): String {\n\
\x20   if (Yaml(\"a\").inline() != \"a\") return \"FAIL: inline present\"\n\
\x20   if (Yaml(null).inline() != \"none\") return \"FAIL: inline absent\"\n\
\x20   if (Yaml(\"b\").block() != \"b\") return \"FAIL: block present\"\n\
\x20   if (Yaml(null).block() != \"none\") return \"FAIL: block absent\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "extension_receiver_smartcast");
}

/// The corpus shape: a nullable sibling property reached with `!!` in the other branch, and a result
/// type that is a common supertype rather than the property's own.
#[test]
fn the_proof_survives_a_supertype_result_and_a_sibling_read() {
    const MAIN: &str = "class Yaml(val ref: String?, val get: String?)\n\
class Direct(val name: String)\n\
class Derived(val name: String)\n\
fun join(a: String, b: String): String = a + b\n\
fun Yaml.expand(parent: String): Any =\n\
\x20   if (ref != null) {\n\
\x20       Direct(ref)\n\
\x20   } else {\n\
\x20       Derived(join(get!!, parent))\n\
\x20   }\n\
fun box(): String {\n\
\x20   val present = Yaml(\"a\", null).expand(\"p\") as Direct\n\
\x20   if (present.name != \"a\") return \"FAIL: present \" + present.name\n\
\x20   val absent = Yaml(null, \"g\").expand(\"p\") as Derived\n\
\x20   if (absent.name != \"gp\") return \"FAIL: absent \" + absent.name\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "extension_receiver_supertype");
}

/// The spellings that already worked stay working — a member function, and the qualified `this.ref`.
#[test]
fn the_member_and_qualified_spellings_still_smart_cast() {
    const MAIN: &str = "fun take(value: String): String = value\n\
class Yaml(val ref: String?) {\n\
\x20   fun member(): String = if (ref != null) take(ref) else \"none\"\n\
}\n\
fun Yaml.qualified(): String = if (this.ref != null) take(this.ref) else \"none\"\n\
fun explicit(y: Yaml): String = if (y.ref != null) take(y.ref) else \"none\"\n\
fun box(): String {\n\
\x20   if (Yaml(\"a\").member() != \"a\") return \"FAIL: member\"\n\
\x20   if (Yaml(\"b\").qualified() != \"b\") return \"FAIL: qualified\"\n\
\x20   if (explicit(Yaml(\"c\")) != \"c\") return \"FAIL: explicit\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "receiver_spellings_smartcast");
}

/// A `var` is not stable, so no spelling may narrow it — widening the key must not open a hole.
/// Both compilers' output is asserted.
#[test]
fn a_mutable_extension_receiver_property_still_declines() {
    const MAIN: &str = "class Yaml(var ref: String?)\n\
fun take(value: String): String = value\n\
fun Yaml.expand(): String = if (ref != null) take(ref) else \"none\"\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a `var` smart cast: {}",
        result.reference_stderr
    );
    assert!(
        result
            .reference_stderr
            .contains("smart cast to 'String' is impossible, because 'ref' is a mutable property"),
        "unexpected kotlinc output: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a `var` smart cast kotlinc rejects: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
