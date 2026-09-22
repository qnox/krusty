//! An anonymous object declared inside a lambda inside ANOTHER anonymous object's member.
//!
//! The innermost object's capture of an enclosing value was described with the enclosing class as
//! its owner but with the field index the capture has in the object being DECLARED. The two number
//! different field lists, so the read addressed a field the owner does not have:
//!
//! ```text
//! thread 'main' panicked at src/jvm/ir_emit.rs:
//! index out of bounds: the len is 3 but the index is 3
//! ```
//!
//! Two levels of anonymous object with a lambda between them are required; one level, or two
//! levels with no lambda, both numbered consistently and compiled.
//!
//! The differential regression below reaches that shape through a source-declared generic helper,
//! not a stdlib collection operation. This keeps intrinsic recognition out of the proof: kotlinc
//! and krusty compile and run the exact same ordinary generic call fixture.

use super::common;

/// The value actually reaches the innermost object: a fix that merely stopped the panic while
/// reading the wrong field would return the wrong string rather than crash.
#[test]
fn an_anon_object_in_a_lambda_in_an_anon_object_reads_the_right_capture() {
    const SRC: &str = "interface Inner { fun get(): String }\n\
        interface Outer { val inner: Inner }\n\
        fun <Input, Output> carryAcross(value: Input, transform: (Input) -> Output): Output =\n\
        \x20   transform(value)\n\
        fun box(): String {\n\
        \x20   val tag = \"K\"\n\
        \x20   val outer = object : Outer {\n\
        \x20       override val inner: Inner = object : Inner {\n\
        \x20           override fun get(): String =\n\
        \x20               carryAcross(\"O\") { head ->\n\
        \x20                   object : Inner { override fun get(): String = head + tag }\n\
        \x20               }.get()\n\
        \x20       }\n\
        \x20   }\n\
        \x20   return outer.inner.get()\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "NestedAnonObjectOuterAndLambdaCapture");
}

/// The innermost object capturing only the lambda's own parameter panicked identically, so it is
/// pinned separately: it proves the defect was the field NUMBERING, not a missed capture.
#[test]
fn an_anon_object_in_a_lambda_may_capture_only_the_lambda_parameter() {
    const SRC: &str = "interface Inner { fun get(): String }\n\
        interface Outer { val inner: Inner }\n\
        fun <Input, Output> carryAcross(value: Input, transform: (Input) -> Output): Output =\n\
        \x20   transform(value)\n\
        fun box(): String {\n\
        \x20   val outer = object : Outer {\n\
        \x20       override val inner: Inner = object : Inner {\n\
        \x20           override fun get(): String =\n\
        \x20               carryAcross(\"OK\") { value ->\n\
        \x20                   object : Inner { override fun get(): String = value }\n\
        \x20               }.get()\n\
        \x20       }\n\
        \x20   }\n\
        \x20   return outer.inner.get()\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "NestedAnonObjectLambdaOnlyCapture");
}

/// An ordinary source declaration supplies the lambda boundary, so the regression cannot pass by
/// taking a `listOf`/`map`/`joinToString` intrinsic route. The same fixture must return the same
/// successful result under kotlinc and krusty.
#[test]
fn a_source_generic_helper_preserves_the_nested_objects_capture_binding() {
    const SRC: &str = "interface Inner { fun get(): String }\n\
        interface Outer { val inner: Inner }\n\
        fun <Input, Output> carryAcross(value: Input, transform: (Input) -> Output): Output =\n\
        \x20   transform(value)\n\
        fun box(): String {\n\
        \x20   val suffix = \"K\"\n\
        \x20   val outer = object : Outer {\n\
        \x20       override val inner: Inner = object : Inner {\n\
        \x20           override fun get(): String =\n\
        \x20               carryAcross(\"O\") { prefix ->\n\
        \x20                   object : Inner { override fun get(): String = prefix + suffix }\n\
        \x20               }.get()\n\
        \x20       }\n\
        \x20   }\n\
        \x20   return outer.inner.get()\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "NestedAnonObjectGenericHelperCapture");
}
