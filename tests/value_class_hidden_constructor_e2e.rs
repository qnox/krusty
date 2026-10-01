//! A class whose constructor takes a value class hides its primary constructor behind a public
//! accessor with a trailing `DefaultConstructorMarker`. kotlinc counts the slots a constructor has
//! when it lowers value classes: declared parameters, an inner class's outer instance and a local
//! class's captures. An anonymous object's captures and a lambda class's are added later, as
//! carriers, and leave the constructor plain.

use super::common;

const SRC: &str = "@JvmInline value class Text(val string: String)\n\
    @JvmInline value class Count(val n: Int)\n\
    interface Reader { fun read(): String }\n\
    fun local(t: Text): String {\n\
    \x20   class Local { fun bar() = t.string }\n\
    \x20   return Local().bar()\n\
    }\n\
    fun localCount(c: Count): Int {\n\
    \x20   class Counted { fun bar() = c.n }\n\
    \x20   return Counted().bar()\n\
    }\n\
    fun anonymous(t: Text): Reader = object : Reader { override fun read() = t.string }\n\
    fun box(): String {\n\
    \x20   if (local(Text(\"a\")) != \"a\" || localCount(Count(2)) != 2) return \"fail\"\n\
    \x20   if (anonymous(Text(\"b\")).read() != \"b\") return \"fail\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// An inner class of a value class: its outer instance is the value class's carrier.
const INNER_SRC: &str = "@JvmInline value class Box(val n: Int) {\n\
    \x20   @Suppress(\"INNER_CLASS_INSIDE_VALUE_CLASS\")\n\
    \x20   inner class Inner(val y: Int)\n\
    }\n\
    fun inner(): Int = Box(1).Inner(2).y\n";

/// `class`'s constructor declarations, as kotlinc and krusty write them, in classfile order.
fn assert_same_constructors(name: &str, source: &str, class: &str) {
    let built = common::compare_with_kotlinc_plugin(
        name,
        source,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let constructors = |disassembly: &str| {
        disassembly
            .lines()
            .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
            .map(str::trim)
            .filter(|line| line.contains(&format!("{class}(")) && line.ends_with(");"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let reference = constructors(&built.reference);
    assert!(
        !reference.is_empty(),
        "kotlinc declares a constructor of {class}"
    );
    assert_eq!(constructors(&built.krusty), reference, "{class}");
}

#[test]
fn a_local_class_capturing_a_value_class_hides_its_constructor() {
    assert_same_constructors("HiddenConstructor", SRC, "HiddenConstructorKt$local$Local");
    assert_same_constructors(
        "HiddenConstructor",
        SRC,
        "HiddenConstructorKt$localCount$Counted",
    );
}

#[test]
fn an_anonymous_object_capturing_a_value_class_keeps_a_plain_constructor() {
    assert_same_constructors("HiddenConstructor", SRC, "HiddenConstructorKt$anonymous$1");
}

#[test]
fn an_inner_class_of_a_value_class_hides_its_constructor() {
    assert_same_constructors("InnerHiddenConstructor", INNER_SRC, "Box$Inner");
}

#[test]
fn hidden_constructors_run() {
    common::expect_box_same_as_kotlinc(SRC, "HiddenConstructor");
}
