use super::common;

const SOURCE: &str = include_str!("../box_regressions/nestedAnonymousConstructionOrder.kt");

// PLATFORM: complete JVM class inventory, headers and member signatures, including table order.
#[test]
fn nested_anonymous_constructor_arguments_keep_declaration_order_in_the_complete_abi() {
    let classes =
        common::classes_against_kotlinc_module(&[("NestedAnonymousConstructionOrder.kt", SOURCE)]);
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        info.access,
                        info.this_class,
                        info.super_class,
                        info.interfaces(),
                        info.signature,
                        info.inner_classes
                    )
                );
                (name.clone(), (header, common::member_table(bytes)))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        inventory(&classes.krusty),
        inventory(&classes.reference),
        "complete class ABI"
    );
}

// PLATFORM: source declarations and generated continuations share a JVM table-name tie.
#[test]
fn two_source_objects_and_a_continuation_keep_their_reference_table_order() {
    super::assert_module_inner_class_rows(&[(
        "Mixed.kt",
        r#"
suspend fun pause() {}
class Mixed {
    suspend fun go(id: String): String {
        val first = object { override fun toString() = id }
        val second = object { override fun toString() = id }
        pause()
        return first.toString() + second.toString()
    }
}
"#,
    )]);
}

#[test]
fn nested_anonymous_constructor_arguments_keep_their_runtime_result() {
    common::expect_box_same_as_kotlinc(SOURCE, "NestedAnonymousConstructionOrder");
}
