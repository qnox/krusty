//! A `val` with a custom getter that references `field` HAS a backing field (per Kotlin semantics),
//! so it may be assigned exactly once in a constructor — even though it has no initializer. Krusty
//! previously mis-classified it as a computed (field-less) property and rejected the assignment.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn val_with_field_reading_getter_assigned_in_ctor() {
    const SRC: &str = "class A {\n\
        \x20 val value: String\n\
        \x20   get() = field + \"K\"\n\
        \x20 constructor(o: String) { value = o }\n\
        }\n\
        fun box(): String = A(\"O\").value\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("val backing-field getter"), "OK");
}
