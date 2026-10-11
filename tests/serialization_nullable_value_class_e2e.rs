//! `@Serializable` properties of value-class type where a null is in play: a nullable property
//! (`Label?`, `Stamp?`) and a value class whose own carrier is nullable (`Stamp(val raw: String?)`).
//!
//! Krusty writes and reads a value-class element as its underlying value. Each place that crosses
//! between that underlying value and the class's own representation failed to verify:
//!
//! ```text
//! Envelope.<init>(I…Label…SerializationConstructorMarker): Type 'Label' (current frame,
//! stack[1]) is not assignable to 'java/lang/String'          (a `Label?` box stored unconverted)
//! Envelope$$serializer.deserialize: Type 'java/lang/String' (current frame, stack[2]) is not
//! assignable to 'Stamp'                                      (a decoded `Stamp?` carrier passed unboxed)
//! ```
//!
//! and `write$Self` handed a `Stamp` box to the `String` serializer. A nullable element's null is
//! the absent value in both directions, as in kotlinc's output; `Stamp(null)` is written as `null`
//! and a non-null `Stamp` element read from `null` is `Stamp(null)`.
use super::serialization_test_support::both_compilers_box_files;

/// The value classes share the data class's file, so this exercises the element representation
/// alone and not the sibling-file declaration facts.
const MODEL: &str = "import kotlinx.serialization.Serializable\n\
    @JvmInline\n\
    @Serializable\n\
    value class Stamp(val raw: String?)\n\
    @JvmInline\n\
    @Serializable\n\
    value class Label(val raw: String)\n\
    @JvmInline\n\
    @Serializable\n\
    value class Count(val n: Int)\n\
    @Serializable\n\
    data class Envelope(\n\
    \x20   val created: Stamp? = null,\n\
    \x20   val port: Stamp,\n\
    \x20   val label: Label? = null,\n\
    \x20   val name: Label,\n\
    \x20   val count: Count? = null,\n\
    )\n";

const MAIN: &str = "import kotlinx.serialization.json.Json\n\
    fun roundTrip(value: Envelope): String {\n\
    \x20   val text = Json.encodeToString(Envelope.serializer(), value)\n\
    \x20   val back = Json.decodeFromString(Envelope.serializer(), text)\n\
    \x20   return text + \" -> \" + back + \" \" + (back == value)\n\
    }\n\
    fun box(): String {\n\
    \x20   val full = roundTrip(Envelope(Stamp(\"t\"), Stamp(\"p\"), Label(\"l\"), Label(\"n\"), Count(3)))\n\
    \x20   val absent = roundTrip(Envelope(port = Stamp(null), name = Label(\"n\")))\n\
    \x20   val nulls = Json.decodeFromString(\n\
    \x20       Envelope.serializer(),\n\
    \x20       \"{\\\"created\\\":null,\\\"port\\\":null,\\\"label\\\":null,\\\"name\\\":\\\"n\\\",\\\"count\\\":null}\",\n\
    \x20   )\n\
    \x20   return full + \" | \" + absent + \" | \" + nulls\n\
    }\n";

#[test]
fn nullable_value_class_elements_round_trip() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", MODEL), ("Main.kt", MAIN)],
            "nullable_value_class_elements"
        ),
        "{\"created\":\"t\",\"port\":\"p\",\"label\":\"l\",\"name\":\"n\",\"count\":3} -> \
         Envelope(created=Stamp(raw=t), port=Stamp(raw=p), label=Label(raw=l), name=Label(raw=n), \
         count=Count(n=3)) true | \
         {\"port\":null,\"name\":\"n\"} -> \
         Envelope(created=null, port=Stamp(raw=null), label=null, name=Label(raw=n), count=null) true | \
         Envelope(created=null, port=Stamp(raw=null), label=null, name=Label(raw=n), count=null)"
    );
}
