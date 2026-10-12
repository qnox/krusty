//! A property annotated `@Volatile` has an `ACC_VOLATILE` backing field and one annotated
//! `@Transient` an `ACC_TRANSIENT` one, as kotlinc's `computeFieldFlags` sets them: on a class or
//! constructor property, an object property, a companion property hoisted to its outer class and a
//! top-level property. `kotlin.concurrent.Volatile` is the same annotation on the JVM.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
import kotlin.concurrent.Volatile as SharedVolatile\n\
\n\
class Flags(@Volatile var a: Int, @SharedVolatile var b: Long) {\n\
    @Transient\n\
    var c: String = \"\"\n\
\n\
    @kotlin.jvm.Volatile\n\
    var d: Any? = null\n\
\n\
    companion object {\n\
        @Volatile\n\
        var shared: Int = 1\n\
    }\n\
}\n\
\n\
object Single {\n\
    @Volatile\n\
    var counter = 0\n\
}\n\
\n\
@Volatile\n\
var topLevel: String = \"t\"\n\
";

#[test]
fn a_volatile_or_transient_property_marks_its_field() {
    for class in [
        "store/Flags",
        "store/Flags$Companion",
        "store/Single",
        "store/VolatileFieldKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "VolatileField",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
