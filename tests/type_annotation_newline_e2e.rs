//! A NEWLINE between a type-use annotation and the type it annotates. Kotlin's grammar is
//! `annotation NL*`, so a wrapped type-argument list may put the annotation on its own line:
//!
//! ```kotlin
//! val xs: List<
//!     @Ann(with = S::class)
//!     Outer.Inner,
//! > = emptyList()
//! ```
//!
//! `parse_type_atom` consumed the leading annotations but did not skip the line break before the type,
//! so it reported "expected a type" / "expected '>'" and then ran to end-of-file. (In a multi-file
//! compile those cascaded errors were also attributed to a DIFFERENT file's EOF, which is what made the
//! failure hard to localise.) Annotation and type on the SAME line always worked.

use super::common;

#[test]
fn newline_between_type_annotation_and_type_parses() {
    let src = "annotation class Ann(val with: kotlin.reflect.KClass<*>)\n\
        class Ser\n\
        class Outer { class Inner }\n\
        val a: List<\n\
        \x20   @Ann(with = Ser::class)\n\
        \x20   Outer.Inner,\n\
        > = emptyList()\n\
        val b: @Ann(with = Ser::class)\n\
        \x20   Outer.Inner = Outer.Inner()\n\
        fun box(): String = if (a.isEmpty() && b is Outer.Inner) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main").expect(
            "a line break between a type-use annotation and its type parses, compiles, runs"
        ),
        "OK"
    );
}

#[test]
fn multiple_annotations_each_on_their_own_line_parse() {
    let src = "annotation class A1\n\
        annotation class A2\n\
        class Foo\n\
        val xs: List<\n\
        \x20   @A1\n\
        \x20   @A2\n\
        \x20   Foo,\n\
        > = emptyList()\n\
        fun box(): String = if (xs.isEmpty()) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main")
            .expect("consecutive type-use annotations separated by line breaks parse"),
        "OK"
    );
}
