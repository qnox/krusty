//! Exact type and inference diagnostic cases against kotlinc.

use super::diagnostics_parity_support::assert_error_parity;

#[test]
fn type_and_inference_errors_match_kotlinc_exactly() {
    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
        // A missing callee is the ordinary UNRESOLVED_REFERENCE diagnostic, not a distinct
        // "unresolved function" error. Cover both function- and constructor-shaped calls.
        "fun use(): Int { noSuchFunction(); return 0 }",
        "fun use(): Any = NoSuchClass(1)",
        // A qualified expression commits to its lexical root before resolving later segments.
        // `java` is the local Int here, not the lower-priority JDK package, so both compilers must
        // diagnose `io` and must not backtrack to `java.io.File`.
        "fun f() { val java = 1; val file = java.io.File(\"A\") }",
        // The same rule applies to a top-level property root: package lookup is considered only when
        // the expression/value scope has no winning declaration named `java`.
        "val java = 1\nfun f() { val file = java.io.File(\"A\") }",
        "class Producer<out T>\nfun bad(value: Producer<in String>) = value",
        "fun f(a: Int): String = a",
        "class Box<T>\nfun <T> bad(x: Box<String>): Box<T> = x",
        "fun f(): String = null",
        "val x: String = null",
        "fun f(x: String): String = x\nfun g(): String = f(null)",
        "fun f() { var x: String = \"\"; x = null }",
        "fun f(): String { return 1 }",
        "fun f(): Int { val x = 1; x = 2; return x }",
        "fun f(x: Int): String { val y: String = x; return y }",
        "val x: String = 1",
        "fun f() { var x: String = \"\"; x = 1 }",
        "fun f(x: Int): Int = x\nfun g(): Int = f()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f<Int>()",
        "fun f(x: Int): Int = x\nfun g(): Int = f ()",
        "fun f(x: Int): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(x: Int): Int = x\nfun g(): Int = f(\"no\")",
        "fun f(): Array<String> = arrayOf()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "suspend fun <T> f(x: T): T = x\nsuspend fun g(): Int = f(1, 2)",
        "inline fun <reified T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T : Any> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T> f(x: T & Any): T & Any = x\nfun g() { f(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(x = null) }",
        "class C<T : CharSequence?>(val x: T)\nfun g() { C(null).x.length }",
        "val String.first: Char get() = 'x'\nfun bad(s: String?): Char = s.first",
        "class PairBox<T>(val a: T, val b: T)\nclass Host { val String.pair get() = PairBox(\"x\", null); fun bad(): Int = \"\".pair.b.length }",
        "class C<T : Number>(val a: T, val b: T)\nfun f() = C(1, \"bad\")",
        "class C<T>(val consume: (T) -> Unit)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume(1) }",
        "class C<T>(val consume: ((T) -> Unit)?)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume!!(1) }",
        "fun <T> inferred(x: T) = x\nfun g(): Int = inferred(1, 2)",
    ];
    assert_error_parity(&cases);
}
