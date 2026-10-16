//! A `@Serializable` value class serializes through its own generated serializer.
//!
//! kotlinc gives a value class a `$$serializer` whose descriptor is an `InlineClassDescriptor` and
//! whose `serialize` is `encoder.encodeInline(descriptor)?.encodeInt(value)`. A class with a property
//! of that type caches the value class's `$$serializer.INSTANCE` as the element's serializer, boxes
//! the property on write, and reads it back as a nullable box that the deserialization constructor
//! takes as the erased nullable value class.
//!
//! krusty instead treated the element as its underlying value: the property was written with the
//! underlying type's serializer and the value class's own serializer was never consulted. Beyond
//! the different bytecode, an unsigned carrier (`UInt`) went through the SIGNED `Int` serializer, so
//! `UInt.MAX_VALUE` was written as `-1`.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;
use super::serialization_test_support::both_compilers_box_files;

/// Two value classes with different carriers (a reference and a primitive), each used as a
/// non-null and a nullable property. Every declaration spans several lines so no two line-table
/// entries share a line.
const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Time(\n\
    \x20   val raw: String,\n\
    )\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Seq(\n\
    \x20   val n: Int,\n\
    )\n\
    \n\
    @Serializable\n\
    data class Event(\n\
    \x20   val at: Time,\n\
    \x20   val until: Time?,\n\
    \x20   val seq: Seq,\n\
    \x20   val next: Seq?,\n\
    )\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Stamp(\n\
    \x20   val raw: String?,\n\
    )\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Wrapped(\n\
    \x20   val seq: Seq,\n\
    )\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Names(\n\
    \x20   val all: List<String>,\n\
    )\n";

fn assert_class_matches_kotlinc(name: &str, class: &str) {
    assert_class_matches_kotlinc_in(name, SOURCE, class);
}

fn assert_class_matches_kotlinc_in(name: &str, source: &str, class: &str) {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(name, source, class, &cp, "25", &extra) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The disassemblies name their own output directories; the class bytes are what must agree.
    if built.krusty_bytes != built.reference_bytes {
        assert_eq!(
            built.krusty.lines().skip(1).collect::<Vec<_>>(),
            built.reference.lines().skip(1).collect::<Vec<_>>(),
            "{class}"
        );
        panic!("{class}: the class bytes differ outside the disassembly");
    }
}

#[test]
fn a_string_backed_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc("SerializableValueClassTime", "Time$$serializer");
}

#[test]
fn an_int_backed_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc("SerializableValueClassSeq", "Seq$$serializer");
}

#[test]
fn a_value_class_property_uses_the_value_classs_serializer() {
    assert_class_matches_kotlinc("SerializableValueClassEvent", "Event$$serializer");
}

/// A nullable carrier is written with `encodeNullableSerializableValue` and its serializer, never
/// with `encodeString`, which rejects a null.
#[test]
fn a_nullable_carrier_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc("SerializableValueClassStamp", "Stamp$$serializer");
}

/// A value-class carrier is written through that value class's own serializer.
#[test]
fn a_value_class_carrier_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc("SerializableValueClassWrapped", "Wrapped$$serializer");
}

/// A collection carrier is built in `serialize`/`deserialize` and cached for `childSerializers`.
#[test]
fn a_collection_carrier_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc("SerializableValueClassNamesSerializer", "Names$$serializer");
}

#[test]
fn a_collection_carrier_value_class_caches_its_child_serializer() {
    assert_class_matches_kotlinc("SerializableValueClassNames", "Names");
}

const CARRIERS_MAIN: &str = "import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val value = Event(Time(\"t\"), null, Seq(1), Seq(2))\n\
    \x20   val event = Json.encodeToString(Event.serializer(), value)\n\
    \x20   val eventBack = Json.decodeFromString(Event.serializer(), event)\n\
    \x20   val stamp = Json.encodeToString(Stamp.serializer(), Stamp(null))\n\
    \x20   val stampBack = Json.decodeFromString(Stamp.serializer(), stamp)\n\
    \x20   val wrapped = Json.encodeToString(Wrapped.serializer(), Wrapped(Seq(3)))\n\
    \x20   val wrappedBack = Json.decodeFromString(Wrapped.serializer(), wrapped)\n\
    \x20   val names = Json.encodeToString(Names.serializer(), Names(listOf(\"a\", \"b\")))\n\
    \x20   val namesBack = Json.decodeFromString(Names.serializer(), names)\n\
    \x20   return event + \" \" + (eventBack == value) + \" \" + stamp + \" \" + stampBack + \" \" +\n\
    \x20       wrapped + \" \" + wrappedBack + \" \" + names + \" \" + namesBack\n\
    }\n";

#[test]
fn every_carrier_round_trips() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", SOURCE), ("Main.kt", CARRIERS_MAIN)],
            "value_class_carriers"
        ),
        "{\"at\":\"t\",\"until\":null,\"seq\":1,\"next\":2} true null Stamp(raw=null) \
         3 Wrapped(seq=Seq(n=3)) [\"a\",\"b\"] Names(all=[a, b])"
    );
}

const GENERIC_MODEL: &str = "import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    @JvmInline\n\
    value class Boxed<T>(\n\
    \x20   val value: T,\n\
    )\n\
    @Serializable\n\
    @JvmInline\n\
    value class Grouped<K, V>(\n\
    \x20   val value: Map<K, List<V>>,\n\
    )\n\
    @Serializable\n\
    class Holder<T>(\n\
    \x20   val boxed: Boxed<T>,\n\
    )\n";

const GENERIC_MAIN: &str = "import kotlinx.serialization.builtins.serializer\n\
    import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val boxed = Boxed.serializer(Int.serializer())\n\
    \x20   val grouped = Grouped.serializer(String.serializer(), Int.serializer())\n\
    \x20   val holder = Holder.serializer(String.serializer())\n\
    \x20   val a = Json.encodeToString(boxed, Boxed(4))\n\
    \x20   val b = Json.encodeToString(grouped, Grouped(mapOf(\"k\" to listOf(1, 2))))\n\
    \x20   val c = Json.encodeToString(holder, Holder(Boxed(\"h\")))\n\
    \x20   return a + \" \" + Json.decodeFromString(boxed, a).value + \" \" + b + \" \" +\n\
    \x20       Json.decodeFromString(grouped, b).value + \" \" + c + \" \" +\n\
    \x20       Json.decodeFromString(holder, c).boxed.value\n\
    }\n";

#[test]
fn a_generic_serializer_round_trips_through_its_type_serializers() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", GENERIC_MODEL), ("Main.kt", GENERIC_MAIN)],
            "generic_value_class_serializers"
        ),
        "4 4 {\"k\":[1,2]} {k=[1, 2]} {\"boxed\":\"h\"} h"
    );
}

/// A generic serializer's private constructor builds the descriptor; the public one takes the
/// type-parameter serializers, delegates to it and stores them in synthetic `KSerializer<*>`
/// fields, which private synthetic getters read.
#[test]
fn a_generic_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableGenericValueClassBoxed",
        GENERIC_MODEL,
        "Boxed$$serializer",
    );
}

#[test]
fn a_two_parameter_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableGenericValueClassGrouped",
        GENERIC_MODEL,
        "Grouped$$serializer",
    );
}

const UNSIGNED_MODEL: &str = "import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    @JvmInline\n\
    value class Mask(val bits: UInt)\n\
    @Serializable\n\
    data class Filter(val mask: Mask, val spare: Mask? = null, val raw: UInt = 1u)\n";

/// The unsigned values are printed through `UInt.toString`, not through the classes' generated
/// `toString`, so this observes serialization alone.
const UNSIGNED_MAIN: &str = "import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val value = Filter(Mask(UInt.MAX_VALUE), Mask(7u), UInt.MAX_VALUE - 1u)\n\
    \x20   val text = Json.encodeToString(Filter.serializer(), value)\n\
    \x20   val back = Json.decodeFromString(Filter.serializer(), text)\n\
    \x20   val mask = Json.encodeToString(Mask.serializer(), Mask(UInt.MAX_VALUE))\n\
    \x20   val maskBack = Json.decodeFromString(Mask.serializer(), mask)\n\
    \x20   return text + \" \" + back.mask.bits.toString() + \" \" + back.spare?.bits.toString() +\n\
    \x20       \" \" + back.raw.toString() + \" \" + (back == value) + \" \" + mask + \" \" +\n\
    \x20       maskBack.bits.toString()\n\
    }\n";

#[test]
fn an_unsigned_value_class_round_trips_unsigned() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", UNSIGNED_MODEL), ("Main.kt", UNSIGNED_MAIN)],
            "unsigned_value_class_element"
        ),
        "{\"mask\":4294967295,\"spare\":7,\"raw\":4294967294} 4294967295 7 4294967294 true \
         4294967295 4294967295"
    );
}

#[test]
fn an_unsigned_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassMask",
        UNSIGNED_MODEL,
        "Mask$$serializer",
    );
}

#[test]
fn an_unsigned_element_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassFilter",
        UNSIGNED_MODEL,
        "Filter$$serializer",
    );
}
