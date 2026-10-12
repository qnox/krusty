//! kotlinc's `SyntheticAccessorLowering` creates a synthetic accessor when it meets the access and
//! only then transforms the access's operands, so an owner declares a store's setter before the
//! getter that the stored value reads (`counter = counter + 1`).

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
private var counter = 0\n\
private var label = \"a\"\n\
\n\
class Tally {\n\
    fun bump() { counter = counter + 1 }\n\
    fun both() { label = label + counter }\n\
}\n\
\n\
class Gauge {\n\
    private var level = 1\n\
\n\
    class Probe {\n\
        fun raise(gauge: Gauge) { gauge.level = gauge.level + 1 }\n\
    }\n\
\n\
    companion object {\n\
        private var peak = 0\n\
    }\n\
\n\
    class Recorder {\n\
        fun record() { peak = peak + 2 }\n\
    }\n\
}\n\
";

#[test]
fn a_store_declares_its_setter_before_the_getter_its_value_reads() {
    for class in ["store/SyntheticAccessorOrderKt", "store/Gauge"] {
        common::byte_diff_against_kotlinc_cp(
            "SyntheticAccessorOrder",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
