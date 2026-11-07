//! Exact argument mapping and overload diagnostic cases against kotlinc.

use super::common;
use super::diagnostics_parity_support::{assert_error_parity, complete_errors};

#[test]
fn argument_mapping_and_overload_errors_match_kotlinc_exactly() {
    let cases = [
        "fun <T> f(x: T, y: Int = 1): T = x\nfun g(): Int = f(1, 2, 3)",
        "fun f(x: Int): Int = x\nfun f(x: String): Int = 0\nfun g(): Int = f(1, 2)",
        "fun f(x: Int = 1): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(a: Int = 0, b: String): String = b\nfun g(): String = f(a = 1)",
        "fun f(`a`: Int = 0, b: String): String = b\nfun g(): String = f(`a` = 1)",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, c = 2, b = \"ok\")",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, a = 2, b = \"ok\")",
        "fun f(a: Int = 0, b: String, vararg x: Int): Int = 0\nfun g(): Int = f()",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f()\n}",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f(1, 2)\n}",
        "fun g(): Int {\nfun choose(a: Int): Int = a\nfun choose(a: String, b: String, c: String): Int = 0\nreturn choose(1, 2)\n}",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f()",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun f(x: Int = 1): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun <T> choose(a: T): T = a; fun <T> choose(a: T, b: T): T = a }\nfun g(c: C): Int = c.choose(1, 2, 3)",
        "class C { fun choose(a: Int): Int = a; fun choose(a: String, b: String, c: String): Int = 0 }\nfun g(c: C): Int = c.choose(1, 2)",
        "open class Base { fun <T> choose(a: T): T = a }\nclass Child : Base()\nfun g(c: Child): Int = c.choose(1, 2)",
        "class C(val x: Int)\nfun g(): C = C()",
        "class C(val x: Int)\nfun g(): C = C(1, 2)",
        "class C(val x: Int = 1)\nfun g(): C = C(1, 2)",
        "class C(val a: Int) { constructor(a: String, b: String, c: String) : this(0) }\nfun g(): C = C(1, 2)",
        "fun f(vararg x: Int): Int = x.size\nfun g(): Int = f(\"no\")",
        "fun f(x: Int = \"no\"): Int = x",
    ];
    assert_error_parity(&cases);
}

#[test]
fn inapplicable_function_value_diagnostics_match_kotlinc_completely() {
    let cases = [
        "fun f(n: Int, label: (String) -> Unit) { label(n) }",
        "fun label(x: Boolean) {}\nfun f(n: Int, label: (String) -> Unit) { label(n) }",
        "class R { fun label(x: Boolean) {} }\nfun f(n: Int, label: (String) -> Unit) { with(R()) { label(n) } }",
    ];
    let stdlib = common::stdlib_jar();
    for (index, source) in cases.iter().enumerate() {
        let file = format!("lexical_value_{index}.kt");
        let result = common::compiler_diagnostics(
            &[(file.as_str(), *source)],
            std::slice::from_ref(&stdlib),
        );
        assert_ne!(result.reference_code, 0, "reference accepted {source:?}");
        assert_ne!(result.krusty_code, 0, "krusty accepted {source:?}");
        let mut observed = complete_errors(&result.krusty_stderr, false);
        observed.extend(complete_errors(&result.krusty_stdout, false));
        let expected = complete_errors(&result.reference_stderr, true);
        assert!(
            !expected.is_empty(),
            "reference emitted no errors for {source:?}"
        );
        assert_eq!(
            observed, expected,
            "complete diagnostic mismatch for {source:?}"
        );
    }
}
