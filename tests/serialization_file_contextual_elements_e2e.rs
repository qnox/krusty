//! Serializers for elements whose type the file's `@UseContextualSerialization` names.
//!
//! kotlinc serializes such an element through `ContextualSerializer(T::class, fallback, [type
//! arguments])`, the same constructor it uses for a contextual property, and caches it in the
//! class's `$childSerializers`. Each test compares the generated code with the reference compiler.
use super::common::{compare_with_kotlinc_plugin, method_instructions};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// A contextual element INSIDE a collection. `@file:UseContextualSerialization(T::class)` makes every
/// `T` in the file serialize through a `ContextualSerializer`, and krusty applied that rule to a
/// PROPERTY of type `T` only — a `List<T>` names no such property, so its element serializer was
/// underivable and the file was declined. Generated clients use exactly this to carry loosely-typed
/// JSON maps.
#[test]
fn a_contextual_element_inside_a_collection_is_derivable() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "@file:UseContextualSerialization(Flexible.FlexibleMap::class)\n\
               import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.UseContextualSerialization\n\
               object Flexible {\n\
               \x20   class FlexibleMap\n\
               }\n\
               @Serializable\n\
               data class Options(val tiers: List<Flexible.FlexibleMap>? = null)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "ContextualCollectionElement",
        src,
        "Options$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "childSerializers()");
    assert_eq!(
        want,
        vec![
            "0: invokestatic # // Method Options.access$get$childSerializers$cp:()[Lkotlin/Lazy;",
            "3: astore_1",
            "4: iconst_1",
            "5: anewarray # // class kotlinx/serialization/KSerializer",
            "8: astore_2",
            "9: aload_2",
            "10: iconst_0",
            "11: aload_1",
            "12: iconst_0",
            "13: aaload",
            "14: invokeinterface # 1 // InterfaceMethod kotlin/Lazy.getValue:()Ljava/lang/Object;",
            "19: checkcast # // class kotlinx/serialization/KSerializer",
            "22: invokestatic # // Method kotlinx/serialization/builtins/BuiltinSerializersKt.getNullable:(Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/KSerializer;",
            "25: aastore",
            "26: aload_2",
            "27: areturn",
        ],
        "kotlinc contextual collection element contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "childSerializers()"),
        want,
        "krusty reads the contextual collection element out of the same cache, and wraps the \
         property's own nullability at the use site exactly where kotlinc does"
    );
}

/// The serializer a file-contextual element is built with. kotlinc constructs the same
/// three-argument `ContextualSerializer(T::class, fallback, [type arguments])` it uses for a
/// contextual property, for a collection element, a direct property and a nullable element alike;
/// a nullable element is made nullable around it, not inside its fallback.
#[test]
fn a_file_contextual_element_uses_the_three_argument_contextual_serializer() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "@file:UseContextualSerialization(Flexible.FlexibleMap::class)\n\
               import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.UseContextualSerialization\n\
               object Flexible {\n\
               \x20   class FlexibleMap\n\
               }\n\
               @Serializable\n\
               data class Options(\n\
               \x20   val tiers: List<Flexible.FlexibleMap>? = null,\n\
               \x20   val one: Flexible.FlexibleMap,\n\
               \x20   val maybe: List<Flexible.FlexibleMap?>,\n\
               )\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "FileContextualElementSerializer",
        src,
        "Options",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for lambda in [
        "_childSerializers$_anonymous_(",
        "_childSerializers$_anonymous_$0(",
        "_childSerializers$_anonymous_$1(",
    ] {
        let want = method_instructions(&built.reference, lambda);
        assert!(
            want.iter().any(|line| line.contains("ContextualSerializer.\"<init>\":(Lkotlin/reflect/KClass;Lkotlinx/serialization/KSerializer;[Lkotlinx/serialization/KSerializer;)V")),
            "kotlinc contract for {lambda}: {want:#?}"
        );
        assert_eq!(method_instructions(&built.krusty, lambda), want, "{lambda}");
    }
}
