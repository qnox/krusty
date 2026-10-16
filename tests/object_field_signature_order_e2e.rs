//! A named object's backing fields are declared at kotlinc's field-table visit.
//!
//! kotlinc visits the field table after `<clinit>` and the accessors, so a backing field's generic
//! `Signature` interns after the initializer's constants. The fields still precede the object's
//! other static storage, such as a generated `$cachedSerializer$delegate`.

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
