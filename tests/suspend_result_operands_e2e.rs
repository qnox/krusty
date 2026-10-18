//! Where a suspending function hands its results on: a string template takes a suspension point's
//! result in the erased `Object` slot the call returns, and a captured `var` in its holder's
//! `element` slot, as kotlinc's `invokedynamic` recipe does; and the machine of a function whose
//! expression body starts on a later line than its signature opens on the signature's line. A
//! discarded library suspension result is popped as the erased `Object` it is.

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

/// A library's suspending member declares `Runtime`, but its descriptor returns the erased
/// `Object`. A statement call discards that erased result at the resumption join, with no cast.
#[test]
fn a_discarded_library_suspend_result_is_popped_uncast() {
    const LIB: &str = "package lib\n\
        class Runtime(val id: String)\n\
        class Backend { suspend fun create(spec: String): Runtime = Runtime(spec) }\n";
    const SOURCE: &str = "import lib.Backend\n\
        fun launch(block: suspend () -> Unit) {}\n\
        class Holder {\n\
        \x20   private val backend = Backend()\n\
        \x20   fun start() = launch {\n\
        \x20       backend.create(\"x\")\n\
        \x20       println(\"y\")\n\
        \x20   }\n\
        }\n";
    let library = common::compile_lib("discarded_library_suspend_result", LIB)
        .expect("krusty builds the library");
    let classpath = [library, common::stdlib_jar()];
    let compared = common::compare_with_kotlinc_plugin(
        "DiscardedResult",
        SOURCE,
        "Holder$start$1",
        &classpath,
        "1.8",
        &[],
    )
    .expect("reference kotlinc and javap must be available");
    assert!(
        compared.krusty_bytes == compared.reference_bytes,
        "Holder$start$1 differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
        compared.reference,
        compared.krusty
    );
}
