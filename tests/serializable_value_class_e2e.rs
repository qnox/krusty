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
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};
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
    )\n\
    @Serializable\n\
    class Counted(\n\
    \x20   val count: Boxed<Int>,\n\
    )\n";

const GENERIC_MAIN: &str = "import kotlinx.serialization.builtins.serializer\n\
    import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val boxed = Boxed.serializer(Int.serializer())\n\
    \x20   val grouped = Grouped.serializer(String.serializer(), Int.serializer())\n\
    \x20   val holder = Holder.serializer(String.serializer())\n\
    \x20   val d = Json.encodeToString(Counted.serializer(), Counted(Boxed(7)))\n\
    \x20   val a = Json.encodeToString(boxed, Boxed(4))\n\
    \x20   val b = Json.encodeToString(grouped, Grouped(mapOf(\"k\" to listOf(1, 2))))\n\
    \x20   val c = Json.encodeToString(holder, Holder(Boxed(\"h\")))\n\
    \x20   return a + \" \" + Json.decodeFromString(boxed, a).value + \" \" + b + \" \" +\n\
    \x20       Json.decodeFromString(grouped, b).value + \" \" + c + \" \" +\n\
    \x20       Json.decodeFromString(holder, c).boxed.value + \" \" + d + \" \" +\n\
    \x20       Json.decodeFromString(Counted.serializer(), d).count.value\n\
    }\n";

#[test]
fn a_generic_serializer_round_trips_through_its_type_serializers() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", GENERIC_MODEL), ("Main.kt", GENERIC_MAIN)],
            "generic_value_class_serializers"
        ),
        "4 4 {\"k\":[1,2]} {k=[1, 2]} {\"boxed\":\"h\"} h {\"count\":7} 7"
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

/// A property of a generic value class is written as the value class's box: `write$Self` passes it
/// to `encodeSerializableElement`, whose value parameter is a type parameter. Its carrier is
/// `Object` too, and krusty unboxed the box it had just made, so the runtime serializer received
/// the bare carrier and failed its cast.
#[test]
fn a_generic_value_class_property_is_written_as_its_box() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializableGenericValueClassHolder",
        GENERIC_MODEL,
        "Holder",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = member_body(&built.reference, "write$Self$");
    assert!(want.len() > 1, "the reference declares write$Self");
    assert_eq!(member_body(&built.krusty, "write$Self$"), want);
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

/// A carrier whose property names its serializer, by `with =`, by `@Contextual`, or on the type
/// itself. The last two declarations each sit on one line, as the annotation does.
const OVERRIDE_MODEL: &str = "import kotlinx.serialization.Contextual\n\
    import kotlinx.serialization.KSerializer\n\
    import kotlinx.serialization.Serializable\n\
    import kotlinx.serialization.descriptors.PrimitiveKind\n\
    import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
    import kotlinx.serialization.encoding.Decoder\n\
    import kotlinx.serialization.encoding.Encoder\n\
    \n\
    object Upper : KSerializer<String> {\n\
    \x20   override val descriptor = PrimitiveSerialDescriptor(\"Upper\", PrimitiveKind.STRING)\n\
    \x20   override fun serialize(encoder: Encoder, value: String) =\n\
    \x20       encoder.encodeString(value.uppercase())\n\
    \x20   override fun deserialize(decoder: Decoder): String = decoder.decodeString().lowercase()\n\
    }\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Shout(\n\
    \x20   @Serializable(with = Upper::class) val text: String,\n\
    )\n\
    \n\
    @Serializable\n\
    @JvmInline\n\
    value class Resolved(\n\
    \x20   @Contextual val text: String?,\n\
    )\n\
    \n\
    @Serializable @JvmInline value class Typed(val text: @Serializable(with = Upper::class) String)\n\
    @Serializable class Point(val x: Int)\n";

const OVERRIDE_MAIN: &str = "import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val shout = Json.encodeToString(Shout.serializer(), Shout(\"ab\"))\n\
    \x20   val shoutBack = Json.decodeFromString(Shout.serializer(), shout)\n\
    \x20   val resolved = Json.encodeToString(Resolved.serializer(), Resolved(\"cd\"))\n\
    \x20   val resolvedBack = Json.decodeFromString(Resolved.serializer(), resolved)\n\
    \x20   val typed = Json.encodeToString(Typed.serializer(), Typed(\"ef\"))\n\
    \x20   val typedBack = Json.decodeFromString(Typed.serializer(), typed)\n\
    \x20   return shout + \" \" + shoutBack.text + \" \" + resolved + \" \" + resolvedBack.text +\n\
    \x20       \" \" + typed + \" \" + typedBack.text\n\
    }\n";

#[test]
fn a_carrier_serializer_named_by_its_property_is_used() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", OVERRIDE_MODEL), ("Main.kt", OVERRIDE_MAIN)],
            "value_class_carrier_serializers"
        ),
        "\"AB\" ab \"cd\" cd \"EF\" ef"
    );
}

#[test]
fn a_property_serializer_on_the_carrier_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassShout",
        OVERRIDE_MODEL,
        "Shout$$serializer",
    );
}

#[test]
fn a_contextual_carrier_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassResolved",
        OVERRIDE_MODEL,
        "Resolved$$serializer",
    );
}

#[test]
fn a_one_line_value_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassTyped",
        OVERRIDE_MODEL,
        "Typed$$serializer",
    );
}

#[test]
fn a_one_line_class_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableValueClassPoint",
        OVERRIDE_MODEL,
        "Point$$serializer",
    );
}

const GENERIC_CONTEXTUAL_MODEL: &str = "import kotlinx.serialization.Contextual\n\
    import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    @JvmInline\n\
    value class Generic<T>(\n\
    \x20   @Contextual val value: T,\n\
    )\n\
    @Serializable\n\
    @JvmInline\n\
    value class Strict<T : Any>(\n\
    \x20   @Contextual val value: T,\n\
    )\n\
    @Serializable\n\
    @JvmInline\n\
    value class Loose<T : Any?>(\n\
    \x20   @Contextual val value: T,\n\
    )\n";

const GENERIC_CONTEXTUAL_MAIN: &str = "import kotlinx.serialization.builtins.serializer\n\
    import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val serializer = Generic.serializer(String.serializer())\n\
    \x20   val text = Json.encodeToString(serializer, Generic(\"gh\"))\n\
    \x20   val back = Json.decodeFromString(serializer, text)\n\
    \x20   return text + \" \" + back.value\n\
    }\n";

#[test]
fn a_generic_contextual_carrier_uses_its_type_parameter_serializer() {
    assert_eq!(
        both_compilers_box_files(
            &[
                ("Model.kt", GENERIC_CONTEXTUAL_MODEL),
                ("Main.kt", GENERIC_CONTEXTUAL_MAIN),
            ],
            "generic_contextual_value_class_carrier"
        ),
        "\"gh\" gh"
    );
}

#[test]
fn a_generic_contextual_carrier_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableGenericContextualValueClass",
        GENERIC_CONTEXTUAL_MODEL,
        "Generic$$serializer",
    );
}

/// A value-class carrier follows the type parameter's resolved bound: unconstrained `T` can carry
/// null and uses nullable serializer calls, while `T : Any` keeps the non-null calls. The existing
/// `Generic<T>` comparison above pins the first half; this pins the contrasting bound.
#[test]
fn a_non_null_bounded_generic_contextual_carrier_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableStrictGenericContextualValueClass",
        GENERIC_CONTEXTUAL_MODEL,
        "Strict$$serializer",
    );
}

/// An explicit nullable upper bound takes the same carrier-coding branch as the implicit nullable
/// bound above, while preserving the declaration's distinct generic signature.
#[test]
fn an_explicit_nullable_bounded_generic_contextual_carrier_serializer_is_kotlincs() {
    assert_class_matches_kotlinc_in(
        "SerializableLooseGenericContextualValueClass",
        GENERIC_CONTEXTUAL_MODEL,
        "Loose$$serializer",
    );
}
