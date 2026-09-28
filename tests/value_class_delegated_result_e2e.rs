//! A class delegating `Map<String, Field>` to another map returns the value class's nullable
//! carrier from its mangled `get`: the delegate's erased `Map.get` result is the box, unboxed
//! null-safely once. The unboxed result kept the box's physical type, so the return unboxed it a
//! second time and the class failed verification.

use super::common;

const SRC: &str = "@JvmInline value class Field(val value: String)\n\
    class One(private val key: String, private val field: Field) : Map<String, Field> {\n\
    \x20   override val size: Int get() = 1\n\
    \x20   override fun isEmpty(): Boolean = false\n\
    \x20   override fun containsKey(key: String): Boolean = key == this.key\n\
    \x20   override fun containsValue(value: Field): Boolean = value == field\n\
    \x20   override fun get(key: String): Field? = if (key == this.key) field else null\n\
    \x20   override val keys: Set<String> get() = throw UnsupportedOperationException()\n\
    \x20   override val values: Collection<Field> get() = throw UnsupportedOperationException()\n\
    \x20   override val entries: Set<Map.Entry<String, Field>> get() = throw UnsupportedOperationException()\n\
    }\n\
    class Params(fields: Map<String, Field>) : Map<String, Field> by fields\n\
    fun box(): String {\n\
    \x20   val params = Params(One(\"k\", Field(\"v\")))\n\
    \x20   if (params[\"missing\"] != null) return \"fail missing\"\n\
    \x20   return if (params[\"k\"]!!.value == \"v\") \"OK\" else \"fail value\"\n\
    }\n";

#[test]
fn a_delegated_value_class_result_is_unboxed_once() {
    let built = common::compare_with_kotlinc_plugin(
        "DelegatedResult",
        SRC,
        "Params",
        &[common::stdlib_jar(), common::jdk_modules()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public java.lang.String get--OyO7hY(java.lang.String)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_delegated_value_class_result_runs() {
    common::expect_box_same_as_kotlinc(SRC, "ValueClassDelegatedResult");
}
