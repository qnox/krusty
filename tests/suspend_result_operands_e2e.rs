//! Where a suspending function hands its results on: a string template takes a suspension point's
//! result in the erased `Object` slot the call returns, and a captured `var` in its holder's
//! `element` slot, as kotlinc's `invokedynamic` recipe does; and the machine of a function whose
//! expression body starts on a later line than its signature opens on the signature's line.

use super::common;

const SOURCE: &str = "suspend fun word(): String = \"b\"\n\
\n\
suspend fun template(): String = \"a\" + word()\n\
\n\
suspend fun spanning(): String =\n\
    word() + word()\n\
\n\
fun launch(block: suspend () -> Unit) {}\n\
\n\
fun box() {\n\
    var result = \"\"\n\
    launch { result += word() }\n\
}\n";

/// Every class is the reference compiler's, byte for byte.
#[test]
fn suspension_results_reach_their_consumers_as_kotlinc_writes_them() {
    for class in ["SuspendResultOperandsKt", "SuspendResultOperandsKt$box$1"] {
        let compared = common::compare_with_kotlinc_plugin(
            "SuspendResultOperands",
            SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        )
        .unwrap_or_else(|| panic!("reference kotlinc and javap must be available"));
        assert!(
            compared.krusty_bytes == compared.reference_bytes,
            "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            compared.reference,
            compared.krusty
        );
    }
}
