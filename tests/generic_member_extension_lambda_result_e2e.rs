//! A generic member EXTENSION's lambda parameter keeps its declared result.
//!
//! `fun <T : Cfg> Holder<T>.engine(block: T.() -> Unit)` is ktor's own shape, and it reaches a
//! different channel from an ordinary member: the member-extension plan carried the lambda's inputs
//! and receiver but recorded no expected type at all, so the declared `Unit` result was lost and the
//! block's last expression was judged in value position. A trailing `when` with no `else` — the
//! shape every configuration DSL is written around — was then reported non-exhaustive on source
//! kotlinc accepts, and the module emitted nothing.
//!
//! The library is compiled by the reference compiler on purpose: the behaviour under test is how
//! krusty reads the parameter's declared shape out of `@Metadata`.

use super::common;

const LIBRARY: &str = "package dsl\n\
    open class Cfg\n\
    class TlsCfg : Cfg() {\n\
    \x20 var trust: String? = null\n\
    }\n\
    class Holder<T : Cfg>(val cfg: T) {\n\
    \x20 fun configure(block: T.() -> Unit) {\n\
    \x20\x20 cfg.block()\n\
    \x20 }\n\
    }\n\
    fun <T : Cfg> Holder<T>.engine(block: T.() -> Unit) {\n\
    \x20 cfg.block()\n\
    }\n\
    fun <T : Cfg> Holder<T>.pick(block: T.() -> String): String = cfg.block()\n";

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

/// The generic member extension whose lambda receiver is the type parameter itself.
#[test]
fn a_generic_member_extension_lambda_is_a_statement_position() {
    assert_both_accept(
        "import dsl.Holder\n\
         import dsl.TlsCfg\n\
         import dsl.engine\n\
         fun use(h: Holder<TlsCfg>, flag: Boolean, ca: String?) {\n\
         \x20 h.engine {\n\
         \x20\x20 when {\n\
         \x20\x20\x20 flag -> { trust = \"insecure\" }\n\
         \x20\x20\x20 ca != null -> { trust = ca }\n\
         \x20\x20 }\n\
         \x20 }\n\
         }\n",
        "a generic member-extension receiver lambda",
    );
}

/// The receiver really is the substituted type argument, so its members resolve.
#[test]
fn the_lambda_receiver_is_the_substituted_type_argument() {
    assert_both_accept(
        "import dsl.Holder\n\
         import dsl.TlsCfg\n\
         import dsl.engine\n\
         fun use(h: Holder<TlsCfg>) {\n\
         \x20 h.engine { trust = \"ca\" }\n\
         }\n",
        "a substituted generic member-extension receiver",
    );
}

/// Control: a non-`Unit` result keeps the body in value position, so a non-exhaustive `when` there
/// is still an error. Both compilers reject; they word it differently, so kotlinc pins the verdict
/// and krusty pins its complete diagnostic output.
#[test]
fn a_non_unit_generic_member_extension_lambda_still_needs_an_exhaustive_when() {
    let Some(library) = common::kotlinc_library(LIBRARY) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "User.kt",
            "import dsl.Holder\n\
             import dsl.TlsCfg\n\
             import dsl.pick\n\
             fun use(h: Holder<TlsCfg>, flag: Boolean): String =\n\
             \x20 h.pick { when { flag -> \"a\" } }\n",
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
                "{path}:5:12: error: 'when' expression must be exhaustive. Add an 'else' \
                 branch.\nkrusty: 1 error(s)\n"
            )
            .as_str()
        ),
        "krusty must still reject a non-exhaustive `when` in a value position"
    );
    let _ = std::fs::remove_dir_all(library);
}

/// The runtime contract: the block runs against the substituted receiver.
#[test]
fn the_generic_member_extension_block_runs_against_its_receiver() {
    let Some(out) = common::expect_box_run_against_kotlinc(
        LIBRARY,
        "import dsl.Holder\n\
         import dsl.TlsCfg\n\
         import dsl.engine\n\
         fun box(): String {\n\
         \x20 val h = Holder(TlsCfg())\n\
         \x20 h.engine { trust = \"ca\" }\n\
         \x20 return if (h.cfg.trust == \"ca\") \"OK\" else \"${h.cfg.trust}\"\n\
         }\n",
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(out, "OK");
}

/// The same relation reached through a MEMBER whose lambda receiver is the CLASS's type parameter.
/// That is ktor's own `HttpClientConfig<T>.engine(block: T.() -> Unit)`, and it arrives on a third
/// carrier again — the provider expectation — which is where the result had to be read.
#[test]
fn a_member_lambda_typed_by_a_class_type_parameter_is_a_statement_position() {
    assert_both_accept(
        "import dsl.Holder\n\
         import dsl.TlsCfg\n\
         fun use(h: Holder<TlsCfg>, flag: Boolean, ca: String?) {\n\
         \x20 h.configure {\n\
         \x20\x20 when {\n\
         \x20\x20\x20 flag -> { trust = \"insecure\" }\n\
         \x20\x20\x20 ca != null -> { trust = ca }\n\
         \x20\x20 }\n\
         \x20 }\n\
         }\n",
        "a member lambda typed by a class type parameter",
    );
}
