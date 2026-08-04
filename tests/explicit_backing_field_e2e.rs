use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn typed_backing_field_initialized_in_init() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Holder {\n\
    val numbers: List<Int> field: MutableList<Int>\n\
    init {\n\
        numbers = mutableListOf(1, 2, 3)\n\
    }\n\
}\n\
fun box(): String = when {\n\
    Holder().numbers == listOf(1, 2, 3) -> \"OK\"\n\
    else -> \"Fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed field in init"), "OK");
}

// The in-owner widening this asserts was never implemented: inside `Inventory`, `items` should read as
// the FIELD type (`MutableList<String>`), and it reads as the PROPERTY type (`List<String>`) instead, so
// `items.add(…)` does not resolve. The test passed only because `kotlin/collections/List` used to inherit
// `java.util.List`'s whole method set — including `add` — which its Kotlin API does not declare. Sourcing
// a mapped collection's scope from `.kotlin_builtins` removed that leak and exposed the gap; the TYPED
// form (`field: MutableList<String>`) fails identically, so neither spelling ever worked. `storage_ty` is
// computed in `resolve.rs` but never reaches member lookup.
#[test]
fn inferred_backing_field_gap_declines_without_a_java_scope_leak() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Inventory {\n\
    val items: List<String>\n\
        field = mutableListOf<String>()\n\
    fun add(item: String) {\n\
        items.add(item)\n\
    }\n\
}\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved") && message.contains("add")),
        "the unsupported backing-field widening must decline explicitly: {diagnostics:?}"
    );
}

#[test]
fn typed_backing_field_is_visible_inside_owner() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Holder {\n\
    val value: Any\n\
        field: String = \"OK\"\n\
    fun read(): String = requireString(value)\n\
}\n\
fun requireString(value: String): String = value\n\
fun box(): String = Holder().read()\n";
    assert_eq!(run(SRC).expect("narrowed inside read"), "OK");
}
