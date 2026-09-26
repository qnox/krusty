//! The order kotlinc visits a class's members when it serializes `@Metadata`, measured against
//! kotlinc 2.4.20.
//!
//! Source declarations come first, in declaration order, and the members the compiler generates
//! for a data or value class follow them. Enum entries are visited in their declaration position,
//! after the primary-constructor properties and before the body's other members. The visit order
//! decides both the order within each protobuf list and the order strings enter `d2`.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_data_class_visits_its_declared_members_before_the_generated_ones() {
    let src = "package app\n\
        \n\
        data class Pair2(val first: Int, val second: Long) {\n\
        \x20   val left: Int get() = first\n\
        \x20   fun swap(): Pair2 = this\n\
        \x20   var note: String = \"\"\n\
        \x20   constructor(only: Int) : this(only, 0L)\n\
        }\n";
    assert_identical("DataOrder", src, "app/Pair2");
}

#[test]
fn a_value_class_visits_its_declared_members_before_the_generated_ones() {
    let src = "package app\n\
        \n\
        @JvmInline\n\
        value class Name(val text: String) {\n\
        \x20   val same: Name get() = this\n\
        \x20   fun upper(): Name = this\n\
        }\n";
    assert_identical("ValueOrder", src, "app/Name");
}

#[test]
fn enum_entries_are_visited_before_the_body_members() {
    let src = "package app\n\
        \n\
        annotation class Mark\n\
        \n\
        enum class Plain {\n\
        \x20   ONE, @Mark TWO;\n\
        \x20   val label: Int get() = 1\n\
        \x20   fun next(): Plain = this\n\
        }\n";
    assert_identical("EnumOrder", src, "app/Plain");
}

#[test]
fn enum_entries_are_visited_after_the_constructor_properties() {
    let src = "package app\n\
        \n\
        enum class Coded(val code: Int, val alias: String) {\n\
        \x20   A(1, \"a\"), B(2, \"b\");\n\
        \x20   fun describe(): String = alias\n\
        \x20   val twice: Int get() = code\n\
        }\n";
    assert_identical("EnumCtorOrder", src, "app/Coded");
}
