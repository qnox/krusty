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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "qualified_call_receiver_lambda");
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
    common::expect_box_ok_files_with_stdlib(
        &[("Dep.kt", DEP), ("Main.kt", MAIN)],
        "cross_package_qualified_receiver_lambda",
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "other_call_spellings");
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
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject an unknown member: {}",
        result.reference_stderr
    );
    assert!(
        result
            .reference_stderr
            .contains("unresolved reference 'missing'"),
        "unexpected kotlinc output: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted an unknown member: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
