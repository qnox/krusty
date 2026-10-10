//! A member function's `reified` type parameter in `@Metadata`.
//!
//! kotlinc writes `TypeParameter.reified` for a member function's reified parameter as it does for
//! a top-level function's. A consumer reads the flag to decide that a call must be inlined with its
//! type argument; without it a separately compiled caller emits a plain call, which throws at run
//! time.

use super::common;

const LIB: &str = "package dep\n\
open class Node\n\
class Leaf : Node()\n\
class Filter {\n\
\x20   inline fun <reified T : Node> count(items: List<Node>): Int = items.count { it is T }\n\
\x20   inline fun <reified T> first(items: List<Any>): T? = items.firstOrNull { it is T } as? T\n\
}\n";

#[test]
fn member_reified_type_parameters_are_recorded() {
    let classpath = [common::stdlib_jar()];
    let result = common::metadata_diff_against_kotlinc_cp("Filter", LIB, "dep/Filter", &classpath)
        .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_separately_compiled_caller_inlines_a_member_reified_function() {
    const MAIN: &str = "import dep.*\n\
fun box(): String {\n\
\x20   val filter = Filter()\n\
\x20   val items = listOf(Leaf(), Node(), Leaf())\n\
\x20   if (filter.count<Leaf>(items) != 2) return \"count\"\n\
\x20   if (filter.first<Leaf>(items) !== items[0]) return \"first\"\n\
\x20   if (filter.first<String>(items) != null) return \"none\"\n\
\x20   return \"OK\"\n\
}\n";
    let Some(out) = common::expect_box_run_against_ref("reified_member_metadata", LIB, MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
