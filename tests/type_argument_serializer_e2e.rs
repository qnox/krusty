//! A `@Serializable(with = S::class)` written on a TYPE ARGUMENT of a serializable property selects
//! that argument's serializer.
//!
//! ```kotlin
//! @Serializable
//! data class Holder(val items: List<@Serializable(with = ItemSerializer::class) Item>)
//! ```
//!
//! kotlinc builds the element serializer from `S` (`ArrayListSerializer(ItemSerializer.INSTANCE)`)
//! whatever serializer `Item` has, including none. krusty parsed the annotation's argument list and
//! dropped it, so the element derivation looked for `Item`'s own serializer, found none, and the
//! backend declined the whole file with a generic `1:1` error.
//!
//! The same annotation on the declared type itself (`val v: @Serializable(with = S::class) Item`)
//! is the property's own serializer. A serializer `object` from a dependency is read through its
//! `INSTANCE`, never constructed (its constructor is private).

use super::common::{compare_with_kotlinc_plugin, method_instructions};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;
use super::serialization_test_support::{
    both_compilers_box, both_compilers_box_against_dependency,
};

const SERIALIZERS: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.Json\n\
\n\
class Item(val raw: String)\n\
\n\
object ItemObjectSerializer : KSerializer<Item> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"fixture.ItemObject\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeString(value.raw)\n\
\x20   override fun deserialize(decoder: Decoder): Item = Item(decoder.decodeString())\n\
}\n\
\n\
class ItemClassSerializer : KSerializer<Item> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"fixture.ItemClass\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeString(\"c:\" + value.raw)\n\
\x20   override fun deserialize(decoder: Decoder): Item = Item(decoder.decodeString().removePrefix(\"c:\"))\n\
}\n\
\n";

/// A round trip through `Holder.serializer()`: the encoded JSON proves which serializer wrote each
/// element, the decoded values prove the same one read it back.
fn round_trip(holder: &str, value: &str, check: &str) -> String {
    format!(
        "{SERIALIZERS}{holder}\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), {value})\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), json)\n\
\x20   return if ({check}) \"OK \" + json else \"FAIL: \" + json\n\
}}\n"
    )
}

#[test]
fn an_object_serializer_on_a_list_element() {
    let source = round_trip(
        "@Serializable\n\
         data class Holder(val items: List<@Serializable(with = ItemObjectSerializer::class) Item>)\n",
        "Holder(listOf(Item(\"a\"), Item(\"b\")))",
        "back.items.map { it.raw } == listOf(\"a\", \"b\")",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_object_list"),
        "OK {\"items\":[\"a\",\"b\"]}"
    );
}

/// A serializer CLASS is constructed with its no-argument constructor.
#[test]
fn a_class_serializer_on_a_map_value() {
    let source = round_trip(
        "@Serializable\n\
         data class Holder(val byName: Map<String, @Serializable(with = ItemClassSerializer::class) Item>)\n",
        "Holder(mapOf(\"k\" to Item(\"v\")))",
        "back.byName[\"k\"]?.raw == \"v\"",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_class_map_value"),
        "OK {\"byName\":{\"k\":\"c:v\"}}"
    );
}

/// A nullable annotated element wraps the named serializer in `.nullable`.
#[test]
fn a_named_serializer_on_a_nullable_element() {
    let source = round_trip(
        "@Serializable\n\
         data class Holder(val maybe: List<@Serializable(with = ItemObjectSerializer::class) Item?>)\n",
        "Holder(listOf(Item(\"n\"), null))",
        "back.maybe.map { it?.raw } == listOf(\"n\", null)",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_nullable_element"),
        "OK {\"maybe\":[\"n\",null]}"
    );
}

/// The annotation is found by its type-argument path, at any depth.
#[test]
fn a_named_serializer_inside_a_nested_generic() {
    let source = round_trip(
        "@Serializable\n\
         data class Holder(\n\
         \x20   val nested: Map<String, List<@Serializable(with = ItemClassSerializer::class) Item>>,\n\
         )\n",
        "Holder(mapOf(\"k\" to listOf(Item(\"x\"))))",
        "back.nested[\"k\"]?.single()?.raw == \"x\"",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_nested_generic"),
        "OK {\"nested\":{\"k\":[\"c:x\"]}}"
    );
}

/// On the declared type itself the annotation names the property's own serializer.
#[test]
fn a_named_serializer_on_the_declared_type_itself() {
    let source = round_trip(
        "@Serializable\n\
         data class Holder(val direct: @Serializable(with = ItemClassSerializer::class) Item)\n",
        "Holder(Item(\"d\"))",
        "back.direct.raw == \"d\"",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_declared_type"),
        "OK {\"direct\":\"c:d\"}"
    );
}

const DEPENDENCY: &str = "package codecs\n\
\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
class Item(val raw: String)\n\
\n\
object ItemSerializer : KSerializer<Item> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"codecs.Item\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeString(value.raw)\n\
\x20   override fun deserialize(decoder: Decoder): Item = Item(decoder.decodeString())\n\
}\n";

/// The private-corpus shape: a dependency's serializer `object` on a type argument wrapped across
/// lines, in a primary-constructor property with a default. The default makes the property's
/// declaration syntax outlive Pass 1's compaction, which must renumber the annotation's argument
/// with every other retained expression.
#[test]
fn a_dependency_object_serializer_on_a_wrapped_type_argument() {
    let main = "import codecs.Item\n\
import codecs.ItemSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Holder(\n\
\x20   val items: List<\n\
\x20       @Serializable(with = ItemSerializer::class)\n\
\x20       Item,\n\
\x20   > = emptyList(),\n\
\x20   val direct: @Serializable(with = ItemSerializer::class) Item,\n\
)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(listOf(Item(\"a\")), Item(\"d\")))\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), json)\n\
\x20   val ok = back.items.single().raw == \"a\" && back.direct.raw == \"d\"\n\
\x20   return if (ok) \"OK \" + json else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box_against_dependency(DEPENDENCY, main, "type_argument_dependency_object"),
        "OK {\"items\":[\"a\"],\"direct\":\"d\"}"
    );
}

/// A PROPERTY-level `@Serializable(with = …)` naming a dependency's `object` reads its `INSTANCE`.
/// krusty constructed it (`new ItemSerializer()`), which the JVM rejects with `IllegalAccessError`
/// because an object's constructor is private.
#[test]
fn a_property_serializer_object_from_a_dependency_is_read_through_its_instance() {
    let main = "import codecs.Item\n\
import codecs.ItemSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Holder(\n\
\x20   @Serializable(with = ItemSerializer::class)\n\
\x20   val single: Item,\n\
)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Item(\"s\")))\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), json)\n\
\x20   return if (back.single.raw == \"s\") \"OK \" + json else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box_against_dependency(DEPENDENCY, main, "property_dependency_object"),
        "OK {\"single\":\"s\"}"
    );
}

const BYTE_SOURCE: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
class Item(val raw: String)\n\
\n\
object ItemObjectSerializer : KSerializer<Item> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"fixture.ItemObject\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeString(value.raw)\n\
\x20   override fun deserialize(decoder: Decoder): Item = Item(decoder.decodeString())\n\
}\n\
\n\
class ItemClassSerializer : KSerializer<Item> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"fixture.ItemClass\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeString(value.raw)\n\
\x20   override fun deserialize(decoder: Decoder): Item = Item(decoder.decodeString())\n\
}\n\
\n\
@Serializable\n\
data class Holder(\n\
\x20   val items: List<@Serializable(with = ItemObjectSerializer::class) Item>,\n\
\x20   val byName: Map<String, @Serializable(with = ItemClassSerializer::class) Item>,\n\
\x20   val maybe: List<@Serializable(with = ItemObjectSerializer::class) Item?>,\n\
)\n";

/// kotlinc keeps each annotated collection in the `$childSerializers` cache and builds its element
/// from the named serializer: `getstatic ItemObjectSerializer.INSTANCE` for an object,
/// `new ItemClassSerializer` for a class, each narrowed to `KSerializer`, and `.nullable` around a
/// nullable element. The generated `$serializer` is byte-identical.
#[test]
fn type_argument_serializers_match_kotlinc_bytecode() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime are provisioned for byte comparisons");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let serializer = compare_with_kotlinc_plugin(
        "TypeArgumentSerializers",
        BYTE_SOURCE,
        "Holder$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are available");
    assert!(
        serializer.krusty_bytes == serializer.reference_bytes,
        "Holder$$serializer differs from kotlinc:\n--- kotlinc\n{}\n--- krusty\n{}",
        serializer.reference,
        serializer.krusty
    );
    let holder = compare_with_kotlinc_plugin(
        "TypeArgumentSerializers",
        BYTE_SOURCE,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are available");
    for factory in [
        "_childSerializers$_anonymous_()",
        "_childSerializers$_anonymous_$0()",
        "_childSerializers$_anonymous_$1()",
    ] {
        let want = method_instructions(&holder.reference, factory);
        assert!(
            !want.is_empty(),
            "kotlinc emits the {factory} cache factory:\n{}",
            holder.reference
        );
        assert_eq!(
            method_instructions(&holder.krusty, factory),
            want,
            "{factory} builds a different element serializer"
        );
    }
}

const ALIAS_SOURCE: &str = "typealias Named<V> = Map<String, V>\n\
typealias Swapped<V, K> = Map<K, V>\n\
typealias Coded = @Serializable(with = ItemClassSerializer::class) Item\n\
\n\
@Serializable\n\
data class Holder(\n\
\x20   val byName: Named<@Serializable(with = ItemClassSerializer::class) Item>,\n\
\x20   val swapped: Swapped<@Serializable(with = ItemObjectSerializer::class) Item, String>,\n\
\x20   val coded: List<Coded>,\n\
)\n";

/// Below a typealias application the annotation belongs to the expanded type's argument the alias
/// substitutes it into, not to the argument its written index names: `Swapped<@A Item, String>`
/// with `typealias Swapped<V, K> = Map<K, V>` annotates the map's VALUE. An annotation the alias's
/// own right-hand side writes (`typealias Coded = @Serializable(with = S::class) Item`) names the
/// serializer of every `Coded` occurrence. The checked declared spelling records both on the
/// expanded tree, the same one `@Metadata` encodes.
#[test]
fn a_named_serializer_below_a_typealias_follows_the_expansion() {
    let source = round_trip(
        ALIAS_SOURCE,
        "Holder(mapOf(\"k\" to Item(\"v\")), mapOf(\"s\" to Item(\"w\")), listOf(Item(\"l\")))",
        "back.byName[\"k\"]?.raw == \"v\" && back.swapped[\"s\"]?.raw == \"w\" \
         && back.coded.single().raw == \"l\"",
    );
    assert_eq!(
        both_compilers_box(&source, "type_argument_below_typealias"),
        "OK {\"byName\":{\"k\":\"c:v\"},\"swapped\":{\"s\":\"w\"},\"coded\":[\"c:l\"]}"
    );
}

/// kotlinc builds the same cache factories and `$serializer` for the typealias forms as for the
/// spelled-out types: each annotated element is read from its named serializer.
#[test]
fn typealias_type_argument_serializers_match_kotlinc_bytecode() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime are provisioned for byte comparisons");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let source = format!("{SERIALIZERS}{ALIAS_SOURCE}");
    let serializer = compare_with_kotlinc_plugin(
        "TypeAliasArgumentSerializers",
        &source,
        "Holder$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are available");
    assert!(
        serializer.krusty_bytes == serializer.reference_bytes,
        "Holder$$serializer differs from kotlinc:\n--- kotlinc\n{}\n--- krusty\n{}",
        serializer.reference,
        serializer.krusty
    );
    let holder = compare_with_kotlinc_plugin(
        "TypeAliasArgumentSerializers",
        &source,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are available");
    for factory in [
        "_childSerializers$_anonymous_()",
        "_childSerializers$_anonymous_$0()",
        "_childSerializers$_anonymous_$1()",
    ] {
        let want = method_instructions(&holder.reference, factory);
        assert!(
            !want.is_empty(),
            "kotlinc emits the {factory} cache factory:\n{}",
            holder.reference
        );
        assert_eq!(
            method_instructions(&holder.krusty, factory),
            want,
            "{factory} builds a different element serializer"
        );
    }
}
