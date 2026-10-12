//! A dependency member's generic result that is discarded is popped as the descriptor returns it,
//! as kotlinc does: the `checkcast` that reads it as the substituted type exists only for a
//! consumer. The realized call's boundary over the provider's erased result is discarded the same
//! way as a source declaration's result narrowing.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
import java.util.concurrent.atomic.AtomicReference\n\
\n\
fun discard(r: AtomicReference<String>, l: java.util.ArrayList<String>, m: java.util.HashMap<String, Int>) {\n\
    r.getAndSet(\"x\")\n\
    l.set(0, \"y\")\n\
    l.removeAt(0)\n\
    m.put(\"a\", 1)\n\
    m.get(\"a\")\n\
}\n\
\n\
fun consume(r: AtomicReference<String>, l: java.util.ArrayList<String>): String {\n\
    val first = r.getAndSet(\"x\")\n\
    return first + l.set(0, \"y\")\n\
}\n\
";

#[test]
fn a_discarded_dependency_result_is_not_cast() {
    common::byte_diff_against_kotlinc_cp(
        "DiscardedDependencyResult",
        SOURCE,
        "store/DiscardedDependencyResultKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("discarded dependency results differ from kotlinc: {diff}"));
}
