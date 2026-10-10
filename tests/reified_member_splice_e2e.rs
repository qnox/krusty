//! A reified `inline` member function of a dependency class is spliced with the call's type
//! argument, as kotlinc inlines it. The member's declaration publishes its generic signature after
//! a partial view of the same method has already given it a stable identity; the identity has to
//! take that signature, or the reified argument never reaches the inliner.

use super::common;

const LIB: &str = "package dep\n\
open class Node\n\
class Leaf : Node()\n\
class Holder(val nodes: Map<String, Node>) {\n\
\x20   inline fun <reified T> name(): String = T::class.java.simpleName\n\
\x20   inline fun <reified T : Node> find(key: String): T? = nodes[key] as? T\n\
}\n";

const MAIN: &str = "import dep.*\n\
fun box(): String {\n\
\x20   val holder = Holder(mapOf(\"leaf\" to Leaf(), \"node\" to Node()))\n\
\x20   if (holder.name<Leaf>() != \"Leaf\") return \"name: \" + holder.name<Leaf>()\n\
\x20   if (holder.find<Leaf>(\"leaf\") == null) return \"leaf\"\n\
\x20   if (holder.find<Leaf>(\"node\") != null) return \"node\"\n\
\x20   return \"OK\"\n\
}\n";

#[test]
fn a_reified_member_function_of_a_dependency_is_spliced() {
    let Some(out) = common::expect_box_run_against_kotlinc(LIB, MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
