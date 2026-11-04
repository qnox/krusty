//! Kotlin inherits nested classifiers along the superclass chain only. An interface's nested
//! classifier reaches neither an implementing class nor a sub-interface by its simple name, so a
//! same-named top-level classifier binds there instead, and two interfaces that each declare one
//! do not make the name ambiguous.

use super::common;

const IMPLEMENTED_INTERFACES_CONTRIBUTE_NO_CLASSIFIERS: &str = r#"interface Left {
    class Category
}

interface Right {
    class Category
}

open class Base {
    class Kind(val value: String = "inherited")
}

class Category(val value: String = "top")

class Kind(val value: String = "top kind")

class One(val category: Category = Category()) : Left {
    fun body(): Category = Category()
}

class Both(val category: Category = Category()) : Left, Right, Base() {
    fun body(): String = Category().value + "/" + Kind().value
}

fun box(): String {
    val text = One().category.value + "/" + One().body().value + "/" +
        Both().category.value + "/" + Both().body()
    return if (text == "top/top/top/top/inherited") "OK" else text
}
"#;

#[test]
fn implemented_interfaces_contribute_no_nested_classifiers() {
    common::expect_box_same_as_kotlinc(
        IMPLEMENTED_INTERFACES_CONTRIBUTE_NO_CLASSIFIERS,
        "InterfaceNestedClassifierScope",
    );
}

#[test]
fn sub_interface_and_implementing_class_do_not_see_an_interface_nested_classifier() {
    let result = common::compiler_diagnostics(
        &[(
            "Hidden.kt",
            "interface Outer {\n\
             \x20   class Nested\n\
             }\n\
             \n\
             interface Sub : Outer {\n\
             \x20   fun make(): Nested = Nested()\n\
             }\n\
             \n\
             abstract class Impl : Outer {\n\
             \x20   fun make(): Nested = Nested()\n\
             }\n",
        )],
        &[],
    );
    common::expect_identical_rejection(&result, "interface_nested_classifier_scope");
}
