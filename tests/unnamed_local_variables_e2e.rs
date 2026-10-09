//! `UnnamedLocalVariables`: a local `val _` evaluates its initializer and binds nothing. Without the
//! feature kotlinc reports `UNSUPPORTED_FEATURE` at the `_` of every local property, loop variable
//! and `when` subject variable, including those in a local class's members; a destructuring entry,
//! a `catch` parameter and the escaped name `` `_` `` are not unnamed locals. A `var _` has no name
//! to assign through whatever the feature. Each fixture is compiled by both compilers, and the
//! complete error ledger (location, message, count and order) must be the expected one for both.

use super::common;

const UNSUPPORTED: &str = "the feature \"unnamed local variables\" is experimental and should be \
    enabled explicitly. This can be done by supplying the compiler argument \
    '-XXLanguage:+UnnamedLocalVariables', but note that no stability guarantees are provided.";

fn error(at: &str, message: &str) -> String {
    format!("Main.kt:{at}: {message}")
}

/// Require kotlinc and krusty to report exactly `expected` for `source`, kotlinc receiving the
/// fixture's `// LANGUAGE:` directives.
fn assert_ledger(source: &str, expected: &[String]) {
    let sources = [("Main.kt", source)];
    let args = common::language_directives::kotlinc_args(source);
    assert_eq!(
        common::reference_error_ledger(&sources, &args),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(&sources, &args),
        expected
    );
}

const UNNAMED: &str = r#"var calls = 0
fun f(): Int { calls++; return 1 }
fun box(): String {
    val _ = f()
    val _: Int = f()
    for (_ in listOf(1, 2)) { calls++ }
    try { error("x") } catch (_: Exception) { calls++ }
    val (_, b) = 1 to 2
    when (val _ = f()) { else -> calls++ }
    val `_` = 4
    class L { fun m() { val _ = f() } }
    L().m()
    return if (calls == 8 && b == 2 && `_` == 4) "OK" else "fail: $calls"
}
"#;

#[test]
fn unnamed_locals_require_the_feature() {
    assert_ledger(
        UNNAMED,
        &[
            error("4:9", UNSUPPORTED),
            error("5:9", UNSUPPORTED),
            error("6:10", UNSUPPORTED),
            error("9:15", UNSUPPORTED),
            error("11:29", UNSUPPORTED),
        ],
    );
}

#[test]
fn unnamed_locals_run_with_the_feature() {
    let source = format!("// LANGUAGE: +UnnamedLocalVariables\n{UNNAMED}");
    common::expect_box_same_as_kotlinc(&source, "UnnamedLocals");
}

const UNNAMED_VAR: &str = "fun box(): String {\n    var _ = 3\n    return \"OK\"\n}\n";

#[test]
fn an_unnamed_var_requires_the_feature_and_a_name() {
    assert_ledger(
        UNNAMED_VAR,
        &[
            error("2:5", "'var' properties require a name."),
            error("2:9", UNSUPPORTED),
        ],
    );
}

#[test]
fn an_unnamed_var_requires_a_name_with_the_feature() {
    let source = format!("// LANGUAGE: +UnnamedLocalVariables\n{UNNAMED_VAR}");
    assert_ledger(&source, &[error("3:5", "'var' properties require a name.")]);
}
