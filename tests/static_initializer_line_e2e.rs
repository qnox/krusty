//! A static initializer's store opens on its initializer's own line.
//!
//! kotlinc gives a `<clinit>` field store the line of the initializer expression, not of the
//! declaration: `val a: String =\n    make()` stores on the `make()` line. The lines the initializer
//! marks inside itself stay in the table, even an argument on a line of its own that starts at the
//! store's first instruction, where both entries share one offset.

use super::common;

const FACADE: &str = "fun make(): String = \"x\"\n\
    \n\
    val a: String =\n\
    \x20   make()\n\
    \n\
    val b = make()\n\
    \n\
    val d = listOf(\n\
    \x20   \"q\",\n\
    )\n\
    \n\
    fun box(): String = if (a == \"x\" && b == \"x\" && d.single() == \"q\") \"OK\" else \"fail\"\n";

const OBJECT: &str = "fun make(): String = \"x\"\n\
    \n\
    object Settings {\n\
    \x20   val a: String =\n\
    \x20       make()\n\
    \n\
    \x20   val b = make()\n\
    }\n\
    \n\
    object Tags {\n\
    \x20   val d: Any = listOf(\n\
    \x20       \"q\",\n\
    \x20   )\n\
    }\n\
    \n\
    fun box(): String =\n\
    \x20   if (Settings.a == \"x\" && Settings.b == \"x\" && (Tags.d as List<*>).single() == \"q\") \"OK\" else \"fail\"\n";

#[test]
fn a_file_facade_store_opens_on_its_initializer_line() {
    let classes = common::classes_against_kotlinc_module(&[("Facade.kt", FACADE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_file_facade_initializer_runs() {
    common::expect_box_ok_with_stdlib(FACADE, "facade initializer lines");
}

#[test]
fn an_object_store_opens_on_its_initializer_line() {
    let classes = common::classes_against_kotlinc_module(&[("Objects.kt", OBJECT)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_object_initializer_runs() {
    common::expect_box_ok_with_stdlib(OBJECT, "object initializer lines");
}
