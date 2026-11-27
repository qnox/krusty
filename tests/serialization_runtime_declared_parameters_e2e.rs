//! The calls the serialization plugin generates into the serialization runtime carry the runtime
//! declaration's parameters, so a value-class operand reaches a `SerializationStrategy`/`Any?`
//! parameter boxed and a primitive carrier reaches an `Int` parameter unboxed.
//!
//! Each shape below crosses a representation boundary on a plugin-built runtime call: a reified
//! `encodeToString`/`decodeFromString` over a value class, a nested value-class element, a nullable
//! one, and a collection of them in a class's generated serializer.
use super::serialization_test_support::both_compilers_box_files;

const MODEL: &str = "import kotlinx.serialization.Serializable\n\
    @JvmInline\n\
    @Serializable\n\
    value class Seq(val n: Int)\n\
    @JvmInline\n\
    @Serializable\n\
    value class Wrapped(val seq: Seq)\n\
    @Serializable\n\
    data class Batch(\n\
    \x20   val first: Seq,\n\
    \x20   val maybe: Wrapped?,\n\
    \x20   val all: List<Seq>,\n\
    )\n";

const MAIN: &str = "import kotlinx.serialization.encodeToString\n\
    import kotlinx.serialization.json.Json\n\
    fun box(): String {\n\
    \x20   val seq = Json.encodeToString(Seq(3))\n\
    \x20   val seqBack = Json.decodeFromString<Seq>(seq)\n\
    \x20   val wrapped = Json.encodeToString(Wrapped(Seq(4)))\n\
    \x20   val wrappedBack = Json.decodeFromString<Wrapped>(wrapped)\n\
    \x20   val batch = Batch(Seq(1), Wrapped(Seq(2)), listOf(Seq(5), Seq(6)))\n\
    \x20   val text = Json.encodeToString(batch)\n\
    \x20   val back = Json.decodeFromString<Batch>(text)\n\
    \x20   return seq + \" \" + seqBack + \" \" + wrapped + \" \" + wrappedBack + \" \" + text + \" \" + (back == batch)\n\
    }\n";

#[test]
fn plugin_generated_runtime_calls_adapt_value_class_operands() {
    assert_eq!(
        both_compilers_box_files(
            &[("Model.kt", MODEL), ("Main.kt", MAIN)],
            "serialization_runtime_declared_parameters"
        ),
        "3 Seq(n=3) 4 Wrapped(seq=Seq(n=4)) \
         {\"first\":1,\"maybe\":2,\"all\":[5,6]} true"
    );
}
