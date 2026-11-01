//! A named object's backing fields are declared at kotlinc's field-table visit.
//!
//! kotlinc visits the field table after `<clinit>` and the accessors, so a backing field's generic
//! `Signature` interns after the initializer's constants. The fields and the object's `const val`s
//! are declared in source order, and generated static storage, such as a
//! `$cachedSerializer$delegate`, follows them.

use super::common;

const SOURCE: &str = "object Registry {\n\
    \x20 val names: MutableList<String> = ArrayList()\n\
    \x20 val label = \"o\"\n\
    }\n\
    fun box(): String {\n\
    \x20 Registry.names.add(\"OK\")\n\
    \x20 return if (Registry.label == \"o\") Registry.names.single() else \"fail\"\n\
    }\n";

#[test]
fn an_object_field_signature_interns_at_the_field_table_visit() {
    let classes = common::classes_against_kotlinc_module(&[("Registry.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_object_with_a_generic_field_runs() {
    common::expect_box_ok_with_stdlib(SOURCE, "object generic field");
}

const INTERLEAVED_SOURCE: &str = "object Limits {\n\
    \x20 private const val LOW = 3\n\
    \x20 const val MID = 4\n\
    \x20 private val pattern = Regex(\"[a-z]+\")\n\
    \x20 private const val HIGH = 5\n\
    \x20 val names: List<String> = listOf(\"ok\")\n\
    \x20 fun accepts(s: String): Boolean = s.length in LOW..HIGH && pattern.matches(s)\n\
    }\n\
    fun box(): String = if (Limits.accepts(\"okay\") && Limits.MID == 4) Limits.names.single().uppercase() else \"fail\"\n";

#[test]
fn an_object_declares_its_const_and_backing_fields_in_source_order() {
    let classes = common::classes_against_kotlinc_module(&[("Limits.kt", INTERLEAVED_SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_object_with_interleaved_const_fields_runs() {
    common::expect_box_ok_with_stdlib(INTERLEAVED_SOURCE, "object interleaved const fields");
}
