//! Exact receiver, operator, and callable-reference diagnostic cases against kotlinc.

use super::diagnostics_parity_support::assert_error_parity;

#[test]
fn receiver_operator_and_callable_reference_errors_match_kotlinc_exactly() {
    let cases = [
        "fun f(x: String): Int = x.missing",
        "fun f(x: String): Int = x.`missing name`",
        "fun f(x: String?): Int? = x?.`missing name`",
        "fun f(x: String): Int = x.missing()",
        "class C { fun member(value: Int): Int = value }\nfun f(value: C?): Int = value.member(1)",
        "fun f(value: String?): String = value.substring(1)",
        "fun f(value: String?): String = value. /* gap */ substring(1)",
        "fun String.nonNullExtension(): Int = length\nfun f(value: String?): Int = value.nonNullExtension()",
        "class C(val block: () -> Int)\nfun f(value: C?): Int = value.block()",
        "fun <T : Any> T.nonNullGeneric(): Int = 1\nfun f(value: String?): Int = value.nonNullGeneric()",
        "class GenericHolder<T> { fun read(): Int = 1 }\nfun f(value: GenericHolder<String>?): Int = value.read()",
        "fun f(block: (() -> Int)?): Int = block.invoke()",
        "fun f(value: Any?): Boolean = value.equals(null)",
        "fun f(x: String): String = x.substring(\"no\")",
        "fun f(x: Int): Int = x.substring(1)",
        "fun f(): Int { if (1) return 1; return 0 }",
        "fun f(): Int = when { 1 -> 1; else -> 0 }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f() }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f(1, 2) }",
        "class C { fun unaryMinus(): C = this }\nfun g(): C = -C()",
        "class C { fun inc(): C = this }\nfun g(c: C) { var value = c; value++ }",
        "class C { fun plus(other: C): C = this }\nfun g(left: C, right: C): C = left + right",
        "class Parser\nfun Parser.decode(source: String): String = source\nfun Parser.decode(value: Int): String = value.toString()\nfun bad() { val parser = Parser(); val reference = parser::decode }",
        "fun cross(x: String, y: Any): String = \"A\"\nfun cross(x: CharSequence, y: CharSequence): String = \"B\"\nfun bad() { val reference: (String, String) -> String = ::cross }",
        "fun <T> applySame(block: (T) -> T, value: T): T = block(value)\nfun mismatched(x: String, suffix: Char = 'K'): Int = x.length + suffix.code\nfun bad(): Any = applySame(::mismatched, \"O\")",
        "fun foo(x: String, y: Char = 'K'): String = x + y\nfun <T, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold<Int, String>(::foo)",
        "fun foo(x: Int, y: Char = 'K'): String = x.toString() + y\nfun <T : CharSequence, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold(::foo)",
    ];
    assert_error_parity(&cases);
}
