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

/// Compare the COMPLETE diagnostic set of both compilers — count, file, line, column, message and
/// order. A nonzero-exit or substring assertion passes on an unrelated rejection.
fn expect_identical_rejection(result: &common::CompilerDiagnosticResult, tag: &str) {
    let rendered = format!("{}{}", result.krusty_stdout, result.krusty_stderr);
    let krusty = common::compiler_errors(&rendered);
    let reference = common::compiler_errors(&result.reference_stderr);
    assert_ne!(
        result.reference_code, 0,
        "{tag}: kotlinc accepted the fixture: {}",
        result.reference_stderr
    );
    assert_ne!(result.krusty_code, 0, "{tag}: krusty accepted: {rendered}");
    assert!(
        !reference.is_empty(),
        "{tag}: kotlinc rejected with no parseable diagnostic: {}",
        result.reference_stderr
    );
    assert_eq!(
        krusty, reference,
        "{tag}: diagnostics differ.\nkrusty:  {krusty:#?}\nkotlinc: {reference:#?}"
    );
}

/// The failing shape, in both the expression-body and block-body spellings.
/// Run one fixture under BOTH compilers and require the same `box()` value.
///
/// `expect_box_ok_files_with_stdlib` alone only proves krusty agrees with itself. A smart cast that
/// krusty performs and kotlinc does not — or the reverse — is exactly the disagreement these
/// fixtures exist to exclude, so the reference compiler must run the identical source.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

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
    both_compilers_box(MAIN, "extension_receiver_smartcast");
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
    both_compilers_box(MAIN, "extension_receiver_supertype");
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
    both_compilers_box(MAIN, "receiver_spellings_smartcast");
}

/// A `var` is not stable, so no spelling may narrow it — widening the key must not open a hole.
/// Both compilers' output is asserted.
#[test]
fn a_mutable_extension_receiver_property_still_declines() {
    const MAIN: &str = "class Yaml(var ref: String?)\n\
fun take(value: String): String = value\n\
fun Yaml.expand(): String = if (ref != null) take(ref) else \"none\"\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    // Both compilers reject at the same position. They WORD it differently: kotlinc explains why the
    // smart cast is impossible, while krusty reports the resulting type mismatch. That gap is
    // general — krusty has no `mutable property that could be mutated concurrently` wording for any
    // receiver — and predates this change, so it is recorded exactly rather than papered over with a
    // substring match. Converging the wording is its own diagnostic-parity change.
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "krusty writes diagnostics to stderr"
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 3,
            column: 51,
            message: "argument type mismatch: actual type is 'String?', but 'String' was expected."
                .to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 3,
            column: 51,
            message: "smart cast to 'String' is impossible, because 'ref' is a mutable property \
                      that could be mutated concurrently."
                .to_string(),
        }]
    );
}

/// A NEARER receiver shadows an outer one that declares the same property name.
///
/// The proof `ref != null` is about the extension receiver's `ref`. Inside `Inner.run { … }` the
/// nearest receiver is an `Inner`, whose own `ref` is unproven, so the read must NOT find the outer
/// proof. Recorder and read resolve the name through one receiver walk precisely so they cannot
/// disagree about which receiver owns it; this asserts the reference compiler's verdict, record for
/// record.
#[test]
fn a_nearer_receiver_of_the_same_property_name_is_not_proven() {
    const MAIN: &str = "class Outer(val ref: String?)\n\
class Inner(val ref: String?)\n\
fun take(value: String): String = value\n\
fun Outer.probe(inner: Inner): String {\n\
\x20   if (ref != null) {\n\
\x20       return inner.run { take(ref) }\n\
\x20   }\n\
\x20   return \"none\"\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    expect_identical_rejection(&result, "a shadowing nearer receiver");
}
