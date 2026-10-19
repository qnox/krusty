//! A Java generic member specialized by a nullable value-class argument returns that nullable type.
//! `java.util.ArrayList<Meters?>.get` is `(Meters?..Meters?)`, which is `Meters?`, so its value is
//! held boxed and a safe call on it tests it for null, as kotlinc does. krusty typed it `Meters!`,
//! unboxed it to its `int` carrier first and threw a `NullPointerException` on a null element.

use super::common;

const SRC: &str = "@JvmInline value class Meters(val amount: Int) { fun plusOne(): Int = amount + 1 }\n\
    @JvmInline value class Label(val text: String) { fun shout(): String = text + \"!\" }\n\
    fun safeCall(values: java.util.ArrayList<Meters?>): Int? = values[0]?.plusOne()\n\
    fun elvis(values: java.util.ArrayList<Meters?>): Int = values[0]?.amount ?: 7\n\
    fun stored(values: java.util.ArrayList<Meters?>): Meters? { val first = values[0]; return first }\n\
    fun label(values: java.util.ArrayList<Label?>): String? = values.get(0)?.shout()\n\
    fun box(): String {\n\
    \x20   val missing = java.util.ArrayList<Meters?>().also { it.add(null) }\n\
    \x20   if (safeCall(missing) != null || elvis(missing) != 7 || stored(missing) != null) return \"null\"\n\
    \x20   val present = java.util.ArrayList<Meters?>().also { it.add(Meters(2)) }\n\
    \x20   if (safeCall(present) != 3 || elvis(present) != 2 || stored(present) != Meters(2)) return \"value\"\n\
    \x20   val labels = java.util.ArrayList<Label?>().also { it.add(null) }\n\
    \x20   if (label(labels) != null) return \"label\"\n\
    \x20   labels[0] = Label(\"hi\")\n\
    \x20   return if (label(labels) == \"hi!\") \"OK\" else \"label value\"\n\
    }\n";

const NULLABLE: &str = "fun read(values: java.util.ArrayList<String?>): Int = values[0].length\n\
    fun call(values: java.util.ArrayList<String?>): Int = values.get(0).length\n\
    fun stored(values: java.util.ArrayList<String?>): Int { val x = values.get(0); return x.length }\n";

#[test]
fn a_nullable_java_element_keeps_its_value_class_box_and_null_test() {
    let built = common::compare_with_kotlinc_plugin_jdk(
        "ValueClassJavaNullableElement",
        SRC,
        "ValueClassJavaNullableElementKt",
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in [
        "public static final java.lang.Integer safeCall(java.util.ArrayList<Meters>)",
        "public static final int elvis(java.util.ArrayList<Meters>)",
        "public static final Meters stored(java.util.ArrayList<Meters>)",
        "public static final java.lang.String label(java.util.ArrayList<Label>)",
    ] {
        let reference = common::method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc writes {method}");
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn a_null_java_element_of_a_value_class_runs() {
    common::expect_box_same_as_kotlinc(SRC, "ValueClassJavaNullableElement");
}

#[test]
fn a_nullable_java_element_rejects_an_unsafe_call() {
    common::assert_errors_match_kotlinc(&[("main.kt", NULLABLE)], &[]);
}
