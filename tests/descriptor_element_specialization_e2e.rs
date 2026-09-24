//! `ClassSerialDescriptorBuilder.element<T>(…)` is specialized into the non-reified member call.
//!
//! `element` is a reified inline whose body obtains `serializer<T>()` through the kotlinx "magic
//! API" — a `reifiedOperationMarker` with no type-bearing instruction after it. The generic bytecode
//! splice cannot reify that, and because a reified inline has no legal direct-call fallback the
//! refusal bailed the whole file:
//!
//! ```text
//! error: krusty: JVM backend inline error: inline splice failed
//! ```
//!
//! The plugin already knows the serializer, and `ClassSerialDescriptorBuilder` publishes a member
//! taking the descriptor directly, so the call becomes what kotlinc's own inliner emits.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
                   import kotlinx.serialization.descriptors.element\n\
                   @Serializable\n\
                   data class Twig(val id: Int)\n\
                   val descriptor = buildClassSerialDescriptor(\"Twig\") {\n\
                   \x20   element<String>(\"name\")\n\
                   \x20   element<Twig>(\"twig\", isOptional = true)\n\
                   }\n";

fn built() -> (String, String) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "DescriptorElement",
        SRC,
        "DescriptorElementKt",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    (built.reference, built.krusty)
}

/// An unreified marker left in the body is the shape that used to refuse the file; it must not
/// reappear as a spliced-but-unspecialized body either.
#[test]
fn a_descriptor_element_leaves_no_reified_marker() {
    let (reference, krusty) = built();
    for (who, text) in [("kotlinc", &reference), ("krusty", &krusty)] {
        assert!(
            !text.contains("MagicApiIntrinsics") && !text.contains("reifiedOperationMarker"),
            "{who} left an unreified marker in the emitted body:\n{text}"
        );
    }
}

/// The member call itself, with the descriptor of the element's own serializer.
#[test]
fn a_descriptor_element_calls_the_member_with_its_serializer_descriptor() {
    let (reference, krusty) = built();
    for (who, text) in [("kotlinc", &reference), ("krusty", &krusty)] {
        assert!(
            text.contains(
                "ClassSerialDescriptorBuilder.element:(Ljava/lang/String;\
                 Lkotlinx/serialization/descriptors/SerialDescriptor;Ljava/util/List;Z)V"
            ),
            "{who} must call the non-reified member:\n{text}"
        );
        assert!(
            text.contains("KSerializer.getDescriptor"),
            "{who} must pass the element serializer's descriptor:\n{text}"
        );
        assert!(
            text.contains("StringSerializer.INSTANCE"),
            "{who} must resolve the builtin element serializer:\n{text}"
        );
        assert!(
            text.contains("Twig$$serializer.INSTANCE")
                || text.contains("Twig$Companion.serializer"),
            "{who} must resolve the generated element serializer:\n{text}"
        );
    }
}

/// An omitted `annotations` becomes `emptyList()`, and an omitted `isOptional` becomes `false`,
/// while a SUPPLIED `isOptional = true` is kept — a rewrite that ignored the default mask would
/// pass the first two and drop the third.
#[test]
fn a_descriptor_element_materializes_only_the_omitted_defaults() {
    let (reference, krusty) = built();
    for (who, text) in [("kotlinc", &reference), ("krusty", &krusty)] {
        let defaulted = text
            .lines()
            .filter(|line| {
                line.contains("invokestatic") && line.contains("CollectionsKt.emptyList")
            })
            .count();
        assert_eq!(
            defaulted, 2,
            "{who} must default `annotations` at both call sites:\n{text}"
        );
        assert!(
            text.contains("iconst_1"),
            "{who} must keep the supplied `isOptional = true`:\n{text}"
        );
    }
}
