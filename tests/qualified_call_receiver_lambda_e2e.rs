//! A FULLY QUALIFIED call still shapes its receiver lambda.
//!
//! `Json { prettyPrint = true }` binds the lambda's receiver from the selected callable's parameter,
//! so the body resolves `prettyPrint` against `JsonBuilder`. Spelled with its package —
//! `kotlinx.serialization.json.Json { … }` — the receiver never reached the lambda and every member
//! read inside it failed:
//!
//! ```text
//! error: unresolved reference 'prettyPrint'.
//! ```
//!
//! Nothing about the callable differs between the two spellings; only how its name was written. It
//! reproduces with no classpath at all, on a plain source function in the same file.
//!
//! The receiver is not the only thing lost: a qualified call does not shape its lambda argument at
//! all, so an ordinary value lambda's parameter arrives as `Any` too and `a.b.twice(2) { it * 3 }`
//! reports `operator cannot be applied to 'Any' and 'Int'`.

use super::common;

fn both_compilers_box(sources: &[(&str, &str)], stem: &str, reference_main: &str) {
    let reference = common::kotlinc_box_files_result(sources, reference_main);
    common::expect_box_ok_files_with_stdlib(sources, stem);
    assert_eq!(reference, "OK", "{stem}: reference runtime");
}

/// Compare the COMPLETE diagnostic set of both compilers — count, file, line, column, message and
/// order. A nonzero-exit or substring assertion passes on an unrelated rejection, which is how a
/// "both compilers agree" claim goes stale.
fn expect_identical_rejection(result: &common::CompilerDiagnosticResult, tag: &str) {
    let krusty = common::compiler_errors(&result.krusty_stderr);
    let reference = common::compiler_errors(&result.reference_stderr);
    assert_eq!(
        (result.krusty_code, result.reference_code),
        (1, 1),
        "{tag}: unexpected compiler exit status"
    );
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
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

/// The failing shape: a package-qualified call whose parameter is a receiver lambda.
#[test]
fn a_package_qualified_call_binds_its_lambda_receiver() {
    const MAIN: &str = "package a.b\n\
\n\
class Config {\n\
\x20   var flag = false\n\
\x20   var name = \"\"\n\
}\n\
\n\
fun make(block: Config.() -> Unit): Config = Config().apply(block)\n\
\n\
fun qualified(): Config = a.b.make { flag = true; name = \"q\" }\n\
fun bare(): Config = make { flag = true; name = \"b\" }\n\
\n\
fun box(): String {\n\
\x20   val q = qualified()\n\
\x20   if (!q.flag || q.name != \"q\") return \"FAIL: qualified\"\n\
\x20   val b = bare()\n\
\x20   if (!b.flag || b.name != \"b\") return \"FAIL: bare\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(
        &[("Main.kt", MAIN)],
        "qualified_call_receiver_lambda",
        "a.b.MainKt",
    );
}

/// The same spelling reaching a callable in ANOTHER package, and one nested a package deeper — the
/// qualifier length must not matter.
#[test]
fn a_cross_package_qualified_call_binds_its_lambda_receiver() {
    const DEP: &str = "package dep.deeper\n\
\n\
class Builder {\n\
\x20   var size = 0\n\
}\n\
\n\
fun build(block: Builder.() -> Unit): Builder = Builder().apply(block)\n";
    const MAIN: &str = "fun box(): String {\n\
\x20   val built = dep.deeper.build { size = 7 }\n\
\x20   return if (built.size == 7) \"OK\" else \"FAIL: \" + built.size\n\
}\n";
    both_compilers_box(
        &[("Dep.kt", DEP), ("Main.kt", MAIN)],
        "cross_package_qualified_receiver_lambda",
        "MainKt",
    );
}

/// The spellings that already worked stay working, including an ordinary (non-receiver) lambda
/// parameter reached by the same qualified spelling.
#[test]
fn the_other_call_spellings_still_shape_their_lambdas() {
    const MAIN: &str = "package a.b\n\
\n\
class Config {\n\
\x20   var flag = false\n\
}\n\
\n\
fun make(block: Config.() -> Unit): Config = Config().apply(block)\n\
fun twice(value: Int, block: (Int) -> Int): Int = block(value)\n\
\n\
fun box(): String {\n\
\x20   if (!make { flag = true }.flag) return \"FAIL: bare receiver\"\n\
\x20   if (a.b.twice(2) { it * 3 } != 6) return \"FAIL: qualified value lambda\"\n\
\x20   if (twice(2) { it * 3 } != 6) return \"FAIL: bare value lambda\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(&[("Main.kt", MAIN)], "other_call_spellings", "a.b.MainKt");
}

/// A member that does not exist on the receiver is still unresolved — binding the receiver must not
/// turn the lambda body into a permissive scope. Both compilers' output is asserted.
#[test]
fn an_unknown_member_in_the_lambda_is_still_rejected() {
    const MAIN: &str = "package a.b\n\
\n\
class Config {\n\
\x20   var flag = false\n\
}\n\
\n\
fun make(block: Config.() -> Unit): Config = Config().apply(block)\n\
\n\
fun bad(): Config = a.b.make { missing = true }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    expect_identical_rejection(&result, "an unknown member in a shaped lambda");
}

/// An ordinary argument that does not resolve keeps its OWN diagnostic.
///
/// Shaping the lambda means typing the arguments once as a probe, before a candidate is known. That
/// probe's diagnostics are held aside — but only the lambda bodies were judged without a shape. When
/// the whole batch was dropped on selection, an authoritative error from a plain argument went with
/// it, and the call then selected through `Ty::Error`: the reference compiler reported the
/// unresolved name while krusty reported an internal checked-FIR failure and nothing else.
#[test]
fn an_unresolved_ordinary_argument_survives_lambda_shaping() {
    const LIB: &str = "package app.dsl\n\
\n\
class Builder {\n\
\x20   var text: String = \"\"\n\
\x20   fun add(part: String) {\n\
\x20       text += part\n\
\x20   }\n\
}\n\
\n\
fun make(seed: Any, shape: Builder.() -> Unit): String {\n\
\x20   val b = Builder()\n\
\x20   b.text = seed.toString()\n\
\x20   b.shape()\n\
\x20   return b.text\n\
}\n";
    const MAIN: &str = "fun probe(): String = app.dsl.make(missingArgument) { add(\"x\") }\n";
    let result = common::compiler_diagnostics(&[("Lib.kt", LIB), ("Main.kt", MAIN)], &[]);
    expect_identical_rejection(&result, "an unresolved ordinary argument");
}

/// Two function-typed parameters, passed by NAME in the opposite order.
///
/// A named argument names its parameter; its source position says nothing about which one it fills.
/// A second shaping loop that checks lambdas by source position judges each against the OTHER's
/// receiver. The shared selected-call mapper instead commits each argument at its named slot.
#[test]
fn reordered_named_receiver_lambdas_bind_their_own_receivers() {
    const LIB: &str = "package app.dsl\n\
\n\
class Alpha {\n\
\x20   fun onlyAlpha(): String = \"a\"\n\
}\n\
\n\
class Beta {\n\
\x20   fun onlyBeta(): String = \"b\"\n\
}\n\
\n\
fun two(first: Alpha.() -> String, second: Beta.() -> String): String =\n\
\x20   Alpha().first() + Beta().second()\n";
    const MAIN: &str = "fun box(): String {\n\
\x20   val swapped = app.dsl.two(second = { onlyBeta() }, first = { onlyAlpha() })\n\
\x20   return if (swapped == \"ab\") \"OK\" else \"FAIL: \" + swapped\n\
}\n";
    both_compilers_box(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "swapped_named",
        "MainKt",
    );
}

/// A trailing lambda after positional vararg elements must be mapped to the final FUNCTION
/// parameter, not to its source index. The selected-call mapper owns that many-to-one mapping.
#[test]
fn a_trailing_lambda_after_a_vararg_uses_the_selected_parameter_slot() {
    const LIB: &str = "package app.dsl\n\
\n\
class Builder {\n\
\x20   fun onlyBuilder(): String = \"OK\"\n\
}\n\
\n\
fun <T> shape(seed: T, vararg numbers: Int, block: T.() -> String): String =\n\
\x20   seed.block() + if (numbers.sum() == 3) \"\" else \"FAIL: vararg\"\n";
    const MAIN: &str = "fun box(): String =\n\
\x20   app.dsl.shape(app.dsl.Builder(), 1, 2) { onlyBuilder() }\n";
    both_compilers_box(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "qualified_trailing_lambda_after_vararg",
        "MainKt",
    );
}
