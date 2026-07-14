//! A function's block or `=`-expression body may sit on a line AFTER the signature
//! (`fun f(): T\n{ … }`), and a class/interface `where` constraint clause follows the supertype list
//! before the (optional) body. Both are plain-grammar parser cases: line breaks before the body must
//! be skipped, and `parse_where_clause` must run for interfaces too. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn function_block_body_on_next_line() {
    const SRC: &str = "fun f(x: Long, zzz: Long = 1): Long\n\
        {\n\
        \x20 return if (x <= 1) zzz else f(x - 1, x * zzz)\n\
        }\n\
        fun box(): String {\n\
        \x20 return if (f(6) == 720L) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("block body next line"), "OK");
}

#[test]
fn interface_with_where_clause_no_body() {
    const SRC: &str = "interface First\n\
        interface Some<T : First> where T : Some<T>\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(run(SRC).expect("interface where clause"), "OK");
}

#[test]
fn abstract_fun_no_body_still_bodiless() {
    // Regression guard: skipping line breaks before a body must NOT swallow a following member as a
    // body — an abstract method with no body stays bodiless and the next member parses normally.
    const SRC: &str = "abstract class A {\n\
        \x20 abstract fun foo(): Int\n\
        \x20 fun bar(): Int = 7\n\
        }\n\
        class B : A() { override fun foo(): Int = 35 }\n\
        fun box(): String {\n\
        \x20 val b = B()\n\
        \x20 return if (b.foo() + b.bar() == 42) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("abstract fun bodiless"), "OK");
}
