//! A non-null safe-call receiver proves the chain's own root non-null for the rest of the chain.
//!
//! In `a?.f()?.g(a)` the second selector runs only when `a?.f()` is non-null, which entails `a`
//! non-null, so kotlinc smart-casts the argument `a`. Only a stable root narrows: an unstable `var`
//! property keeps its declared type and kotlinc reports the smart cast as impossible.

use super::common;

const PRELUDE: &str = "class Config {\n\
    fun get(key: String): Config? = this\n\
    fun invoke(config: Config): String = \"ok\"\n\
}\n";

fn compile(source: &str) -> common::CompilerDiagnosticResult {
    common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()])
}

#[test]
fn a_stable_root_is_non_null_in_later_selector_arguments() {
    let source = format!(
        "{PRELUDE}fun use(config: Config?): String? =\n  config?.get(\"k\")?.invoke(config)\n"
    );
    let result = compile(&source);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, "")
    );
    assert_eq!((result.krusty_code, result.krusty_stderr.as_str()), (0, ""));
}

#[test]
fn a_root_two_links_back_is_non_null_in_later_selector_arguments() {
    let source = format!(
        "{PRELUDE}fun use(config: Config?): String? =\n  config?.get(\"k\")?.get(\"j\")?.invoke(config)\n"
    );
    let result = compile(&source);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, "")
    );
    assert_eq!((result.krusty_code, result.krusty_stderr.as_str()), (0, ""));
}

#[test]
fn an_unstable_property_root_is_not_narrowed() {
    let source = format!(
        "{PRELUDE}class Holder(var config: Config?) {{\n\
         \x20 fun use(): String? =\n\
         \x20\x20 config?.get(\"k\")?.invoke(config)\n\
         }}\n"
    );
    let result = compile(&source);
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc diagnostic path");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:7:29: error: smart cast to 'Config' is impossible, because 'config' is a mutable property that could be mutated concurrently.\n   config?.get(\"k\")?.invoke(config)\n{}^^^^^^\n",
                " ".repeat(28)
            )
            .as_str()
        )
    );
    let krusty_path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty diagnostic path");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{krusty_path}:7:29: error: argument type mismatch: actual type is 'Config?', but 'Config' was expected.\nkrusty: 1 error(s)\n"
            )
            .as_str()
        )
    );
}
