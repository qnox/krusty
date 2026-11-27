//! A construction that omits a value class's defaulted arguments calls its
//! `constructor-impl$default`. kotlinc marks the construction's line where the operands it
//! synthesizes begin, the placeholders ahead of the mask and marker, whether the value class
//! comes from a library or from another file of the same module. The enclosing constructor's own
//! marker is not one of its defaults: alone, it leaves that line to the `invokespecial`.

use super::common;

const LIBRARY: &str = "package sample\n\
@JvmInline\n\
value class Token(val text: String = \"t\")\n\
@JvmInline\n\
value class Count(val value: Int = 3)\n";

const USE: &str = "import sample.*\n\
class Holder(val token: Token, val name: String)\n\
class Both(val count: Count, val name: String)\n\
fun holder(name: String): Holder =\n\
    Holder(\n\
        token = Token(),\n\
        name = name,\n\
    )\n\
fun both(name: String): Both =\n\
    Both(\n\
        count = Count(),\n\
        name = name,\n\
    )\n";

#[test]
fn a_library_value_class_default_construction_marks_its_line_at_its_synthesized_operands() {
    let Some(classes) = common::classes_against_kotlinc_lib("Use", &[("Lib.kt", LIBRARY)], USE)
    else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    let differences = classes
        .differences()
        .into_iter()
        .filter(|difference| difference.starts_with("UseKt "))
        .collect::<Vec<_>>();
    assert_eq!(differences, Vec::<String>::new());
}

#[test]
fn a_sibling_file_value_class_default_construction_marks_its_line_at_its_synthesized_operands() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", LIBRARY), ("Use.kt", USE)]);
    let differences = classes
        .differences()
        .into_iter()
        .filter(|difference| difference.starts_with("UseKt "))
        .collect::<Vec<_>>();
    assert_eq!(differences, Vec::<String>::new());
}
