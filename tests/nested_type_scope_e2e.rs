//! Unqualified references to a sibling nested TYPE within the enclosing class body — a parameter type
//! (`fun m(i: Inner)`), a local `val v: Inner`, etc. — resolve to `Outer$Inner` (Kotlin nested-type
//! scoping). Construction was already handled; this covers TYPE positions.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nested_type_as_parameter_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun m(i: Inner): String = i.s\n\
    fun go(): String = m(Inner(\"OK\"))\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as parameter"), "OK");
}

#[test]
fn nested_type_as_local_var_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun go(): String { val v: Inner = Inner(\"OK\"); return v.s }\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as local var"), "OK");
}

#[test]
fn nested_type_as_is_smartcast_target() {
    // `is Inner` (and the smart-cast) narrows `a` to the nested type, so `a.v()` resolves.
    const SRC: &str = "class Outer {\n\
    class Inner { fun v() = \"OK\" }\n\
    fun m(a: Any): String = if (a is Inner) a.v() else \"F\"\n\
    fun go(): String = m(Inner())\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as is-smartcast"), "OK");
}

#[test]
fn nested_type_as_cast_target() {
    // `as Inner` on a nested type.
    const SRC: &str = "class Outer {\n\
    class Inner { fun v() = \"OK\" }\n\
    fun m(a: Any): String = (a as Inner).v()\n\
    fun go(): String = m(Inner())\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as cast target"), "OK");
}

#[test]
fn nested_type_as_return_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun mk(): Inner = Inner(\"OK\")\n\
    fun go(): String = mk().s\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as return"), "OK");
}

#[test]
fn inherited_nested_type_in_subclass_constructor() {
    const SRC: &str = "open class Parent {\n\
    enum class Category { FIRST }\n\
}\n\
class Child(val category: Category) : Parent()\n\
fun box(): String = if (Child(Parent.Category.FIRST).category == Parent.Category.FIRST) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("inherited nested constructor type"), "OK");
}

#[test]
fn inherited_nested_type_in_subclass_member() {
    const SRC: &str = "open class Parent {\n\
    class Category(val value: String)\n\
}\n\
class Child : Parent() {\n\
    val category = Category(\"OK\")\n\
}\n\
fun box(): String = Child().category.value\n";
    assert_eq!(run(SRC).expect("inherited nested member type"), "OK");
}

#[test]
fn classpath_protected_nested_type_is_inherited() {
    common::expect_box_ok_against(
        "protected_nested_classifier",
         "package fixtures\n\
         open class Parent {\n\
             protected class Category(private val first: String, private val second: String = \"K\") {\n\
                 fun value(): String = first + second\n\
             }\n\
         }",
        "package fixtures\n\
         class Child : Parent() {\n\
             fun String.read(): String = Category(\"O\").value()\n\
             fun value(): String = \"\".read()\n\
         }\n\
         fun box(): String = Child().value()",
    );
}

#[test]
fn enclosing_subclass_authorizes_nested_class_classifier_scope() {
    common::expect_box_ok_against(
        "nested_lexical_classifier_scope",
        "package support\n\
         open class Parent {\n\
             protected class Category(private val value: String) {\n\
                 fun read(): String = value\n\
             }\n\
         }",
        "package consumer\n\
         import support.Parent\n\
         class Child : Parent() {\n\
             class Nested {\n\
                 private fun use(value: Category): Category = value\n\
                 fun value(): String = use(Category(\"OK\")).read()\n\
             }\n\
             fun value(): String = Nested().value()\n\
         }\n\
         fun box(): String = Child().value()",
    );
}

#[test]
fn inherited_classifier_precedes_same_package_type_in_subclass_scope() {
    common::expect_box_ok_against(
        "inherited_classifier_precedence",
        "package support\n\
         open class Parent {\n\
             protected class Category {\n\
                 fun value(): String = \"OK\"\n\
             }\n\
         }",
        "package consumer\n\
         import support.Parent\n\
         class Category { fun value(): String = \"FAIL\" }\n\
         class Child : Parent() {\n\
             private fun category(value: Category): Category = value\n\
             fun value(): String = category(Category()).value()\n\
         }\n\
         fun box(): String = Child().value()",
    );
}

#[test]
fn inherited_classifier_does_not_expose_defaulted_protected_constructor() {
    let Some(diagnostics) = common::diagnostics_against_ref(
        "protected_default_constructor",
        "package support\n\
         open class Parent {\n\
             protected class Category protected constructor(\n\
                 private val value: String = \"hidden\",\n\
             ) { fun read(): String = value }\n\
         }",
        "package consumer\n\
         import support.Parent\n\
         class Child : Parent() {\n\
             fun value(): String = Category().read()\n\
         }",
    ) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Category")),
        "{diagnostics:?}"
    );
}

#[test]
fn classpath_internal_nested_type_does_not_shadow_source_type() {
    common::expect_box_ok_against_ref(
        "internal_nested_classifier",
        "package fixtures\n\
         open class Parent { internal class Category }",
        "package fixtures\n\
         class Category(val value: String)\n\
         class Child : Parent() { fun value(): String = Category(\"OK\").value }\n\
         fun box(): String = Child().value()",
    );
}
