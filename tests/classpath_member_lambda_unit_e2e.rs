//! A CLASSPATH member function's function-typed parameter keeps its declared `Unit` result.
//!
//! `when { … }` with no `else` is exhaustive only as a STATEMENT. The last expression of a lambda
//! whose declared result is `Unit` is a statement, so a builder block ending in a bare `when` is
//! legal — `engine { https { when { … } } }` is the shape every ktor/Micronaut configuration DSL
//! uses. krusty read the `Unit` result for a classpath TOP-LEVEL function but not for a classpath
//! MEMBER one, so the same `when` inside a member builder became an expression and the file was
//! rejected with "'when' expression must be exhaustive".
//!
//! The library is compiled by the reference compiler on purpose: the behaviour under test is how
//! krusty reads the parameter's declared shape out of `@Metadata`, not how it re-derives it.

use super::common;

const LIBRARY: &str = "package bldr\n\
    class TlsBuilder {\n\
    \x20 var trust: String? = null\n\
    }\n\
    class Engine {\n\
    \x20 fun plain(block: () -> Unit) {}\n\
    \x20 fun tls(block: TlsBuilder.() -> Unit): TlsBuilder = TlsBuilder().apply(block)\n\
    }\n\
    fun topLevelPlain(block: () -> Unit) {}\n\
    fun topLevelTls(block: TlsBuilder.() -> Unit): TlsBuilder = TlsBuilder().apply(block)\n";

/// Compile a user file against the reference-built library with both compilers and require the
/// exact same successful contract.
fn assert_both_accept(user: &str, what: &str) {
    let Some(library) = common::kotlinc_library(LIBRARY) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("User.kt", user)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source kotlinc accepts for {what}"
    );
    let _ = std::fs::remove_dir_all(library);
}

/// A member function taking a plain `() -> Unit`.
#[test]
fn a_classpath_member_lambda_is_a_statement_position() {
    assert_both_accept(
        "import bldr.Engine\n\
         fun use(e: Engine, flag: Boolean) {\n\
         \x20 e.plain { when { flag -> { println(1) } } }\n\
         }\n",
        "a classpath member `() -> Unit`",
    );
}

/// A member function taking a RECEIVER lambda — the builder-DSL shape.
#[test]
fn a_classpath_member_receiver_lambda_is_a_statement_position() {
    assert_both_accept(
        "import bldr.Engine\n\
         fun use(e: Engine, flag: Boolean, ca: String?) {\n\
         \x20 e.tls {\n\
         \x20\x20 when {\n\
         \x20\x20\x20 flag -> { trust = \"insecure\" }\n\
         \x20\x20\x20 ca != null -> { trust = ca }\n\
         \x20\x20 }\n\
         \x20 }\n\
         }\n",
        "a classpath member receiver lambda",
    );
}

/// Control: the classpath TOP-LEVEL spellings already worked and must keep working.
#[test]
fn a_classpath_top_level_lambda_still_is_a_statement_position() {
    assert_both_accept(
        "import bldr.topLevelPlain\n\
         import bldr.topLevelTls\n\
         fun use(flag: Boolean) {\n\
         \x20 topLevelPlain { when { flag -> { println(1) } } }\n\
         \x20 topLevelTls { when { flag -> { trust = \"x\" } } }\n\
         }\n",
        "a classpath top-level lambda",
    );
}

/// Control: a lambda whose declared result is NOT `Unit` keeps its expression position, so a
/// non-exhaustive `when` there is still an error — the fix must not make every lambda a statement.
#[test]
fn a_non_unit_classpath_member_lambda_still_needs_an_exhaustive_when() {
    let Some(library) = common::kotlinc_library(
        "package bldr2\n\
         class Engine {\n\
         \x20 fun pick(block: () -> String): String = block()\n\
         }\n",
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "User.kt",
            "import bldr2.Engine\n\
             fun use(e: Engine, flag: Boolean): String =\n\
             \x20 e.pick { when { flag -> \"a\" } }\n",
        )],
        &classpath,
    );
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must still require an exhaustive `when` in a value position"
    );
    let path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty names the rejected file")
        .to_string();
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{path}:3:12: error: 'when' expression must be exhaustive. Add an 'else' \
                 branch.\nkrusty: 1 error(s)\n"
            )
            .as_str()
        ),
        "krusty must still reject a non-exhaustive `when` in a value position"
    );
    let _ = std::fs::remove_dir_all(library);
}

/// The runtime contract: the block really runs against the built receiver.
#[test]
fn the_member_builder_block_runs_against_its_receiver() {
    let Some(out) = common::expect_box_run_against_kotlinc(
        LIBRARY,
        "import bldr.Engine\n\
         fun box(): String {\n\
         \x20 val built = Engine().tls { trust = \"ca\" }\n\
         \x20 return if (built.trust == \"ca\") \"OK\" else \"${built.trust}\"\n\
         }\n",
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(out, "OK");
}
