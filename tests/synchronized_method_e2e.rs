//! A function annotated `@Synchronized` is an `ACC_SYNCHRONIZED` method and one annotated
//! `@Strictfp` an `ACC_STRICT` one, as kotlinc's `calculateMethodFlags` sets them: on a class
//! member, a property accessor (`@get:`/`@set:`), an object member, a companion member and its
//! `@JvmStatic` bridge, and a top-level function or property accessor. A same-named user
//! annotation is a different identity and changes nothing.

use super::common;

const SOURCE: &str = "\
package sync\n\
\n\
annotation class Synchronized\n\
\n\
class Counter {\n\
    private var n = 0\n\
\n\
    @kotlin.jvm.Synchronized\n\
    fun next(): Int = ++n\n\
\n\
    @Synchronized\n\
    fun decoy(): Int = n\n\
\n\
    @get:kotlin.jvm.Synchronized @set:kotlin.jvm.Synchronized\n\
    var total: Int = 0\n\
\n\
    @Strictfp\n\
    fun half(x: Double): Double = x / 2\n\
\n\
    companion object {\n\
        @JvmStatic @kotlin.jvm.Synchronized\n\
        fun shared(): Int = 1\n\
    }\n\
}\n\
\n\
object Registry {\n\
    @kotlin.jvm.Synchronized\n\
    fun register(name: String): String = name\n\
\n\
    @get:kotlin.jvm.Synchronized\n\
    var held: String = \"\"\n\
}\n\
\n\
@kotlin.jvm.Synchronized\n\
fun topLevel(): Int = 2\n\
\n\
@get:kotlin.jvm.Synchronized @set:kotlin.jvm.Synchronized\n\
var topCount: Int = 0\n\
";

#[test]
fn a_synchronized_or_strictfp_function_marks_its_method() {
    for class in [
        "sync/Counter",
        "sync/Counter$Companion",
        "sync/Registry",
        "sync/SynchronizedMethodKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "SynchronizedMethod",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
