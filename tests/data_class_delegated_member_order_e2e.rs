//! A data class's generated `equals`/`hashCode`/`toString` and its `interface by` forwarders.
//!
//! kotlinc emits the forwarders after the structural members and lists both in one run of the
//! class's metadata, sorted by its declaration comparator, after the declared functions and the
//! components and `copy`: `answer`, `equals`, `greet`, `hashCode`, `toString`. krusty listed the
//! structural members first and the forwarders after them.

use super::common;

const SOURCE: &str = include_str!("box_regressions/dataClassDelegatedMemberOrder.kt");

#[test]
fn data_class_lists_structural_and_delegated_members_as_kotlinc_does() {
    common::assert_class_matches_kotlinc("DataClassDelegatedMemberOrder", SOURCE, "Named");
    common::expect_box_same_as_kotlinc(SOURCE, "DataClassDelegatedMemberOrderRun");
}

// PLATFORM-SPECIFIC: serialization-plugin publication, JVM class bytes and the JVM runtime are
// the contracts here. The original portable data/by program remains in the common box inventory.
const SERIALIZABLE_SOURCE: &str = r#"import kotlinx.serialization.Serializable

interface Greeter {
    fun answer(times: Int): Int
    fun greet(): String
}

@Serializable
class Base : Greeter {
    override fun answer(times: Int): Int = 21 * times
    override fun greet(): String = "O"
}

@Serializable
data class Named(val greeter: Base, val suffix: String) : Greeter by greeter {
    fun own(): String = suffix
}

fun box(): String {
    val named = Named(Base(), "K")
    if (named.answer(2) != 42) return "answer"
    if (named != named.copy()) return "equals"
    if (named.hashCode() != named.copy().hashCode()) return "hashCode"
    return named.greet() + named.own()
}
"#;

#[test]
fn late_serialization_publication_keeps_the_complete_data_delegation_record() {
    let (plugin, cp) = super::serialization_companion_byte_parity_e2e::plugin_and_runtime()
        .expect("the pinned serialization plugin and runtime must be provisioned");
    let extra = [format!("-Xplugin={}", plugin.display())];
    let built = common::compare_with_kotlinc_plugin(
        "SerializableDataDelegatedMemberOrder",
        SERIALIZABLE_SOURCE,
        "Named",
        &cp,
        "25",
        &extra,
    )
    .expect("both compilers and the full class-byte oracle must be available");
    assert_eq!(
        built.krusty_bytes, built.reference_bytes,
        "the complete Serializable data/by class must match kotlinc"
    );
    assert_eq!(
        super::serialization_test_support::both_compilers_box(
            SERIALIZABLE_SOURCE,
            "SerializableDataDelegatedMemberOrder",
        ),
        "OK"
    );
}
