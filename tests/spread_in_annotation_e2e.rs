//! A spread argument (`*arrayOf(...)`) in an annotation-argument list (a `vararg` annotation parameter)
//! parses without error. Annotation values are metadata krusty ignores; the point is that the spread no
//! longer aborts parsing of the annotated declaration. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn spread_arg_in_annotation_parses() {
    const SRC: &str = "annotation class A(vararg val xs: String)\n\
        @A(*arrayOf(\"O\"), \"K\")\n\
        fun box() = \"OK\"\n";
    assert_eq!(run(SRC).expect("spread in annotation"), "OK");
}

#[test]
fn nested_spread_in_annotation_parses() {
    const SRC: &str = "annotation class A(vararg val xs: String)\n\
        annotation class B(vararg val xa: A)\n\
        @B(*arrayOf(A(\"O\", *arrayOf(\"K\")), A()))\n\
        fun box() = \"OK\"\n";
    assert_eq!(run(SRC).expect("nested spread in annotation"), "OK");
}
