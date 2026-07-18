//! Kotlin nested-type scoping: inside a class, an UNQUALIFIED simple name referring to that class's own
//! nested type SHADOWS a same-named top-level (or imported) type. krusty resolved the top-level instead,
//! so a member's field/getter/componentN/copy carried the wrong type — pervasive in the generated
//! httpclient models, where an event inlines a nested `GhMilestoneClient` that shadows the shared
//! top-level one. The checker's `resolve_type` (via `enclosing_nested_type`), the signature-phase
//! class-scope extension, and the lowerer's `field_ty_in` all prefer the nested form consistently.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nested_type_field_shadows_top_level() {
    // `Event.item : Item` resolves to the enclosing `Event`'s nested `Item` (value "N"), NOT the
    // top-level `Item` (value "T"): the field, its getter, componentN, and copy all carry `Event$Item`.
    // `read()` reads through the field (no bare construction of the shadowed type), like the models.
    const SRC: &str = "data class Item(val tag: String)\n\
        class Event {\n\
        \x20 data class Item(val label: String)\n\
        \x20 fun make(): Item = Item(\"N\")\n\
        \x20 fun read(): String = make().label\n\
        }\n\
        fun box(): String = if (Event().read() == \"N\") \"OK\" else \"FAIL:${Event().read()}\"\n";
    assert_eq!(run(SRC).expect("nested-type field shadows top-level"), "OK");
}
