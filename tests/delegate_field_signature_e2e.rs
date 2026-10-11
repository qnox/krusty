//! A delegated property's `x$delegate` storage carries no generic `Signature`, as kotlinc's
//! `PROPERTY_DELEGATE` field origin has none, wherever the storage lives: a file facade, a class,
//! an object or an enum class. A companion's delegate hoisted to its outer class is a new field in
//! kotlinc and keeps its `Signature`.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
import kotlin.reflect.KProperty\n\
\n\
class Box<T>(val value: T) {\n\
    operator fun getValue(owner: Any?, property: KProperty<*>): T = value\n\
}\n\
\n\
val topLevel: String by Box(\"top\")\n\
\n\
class Holder {\n\
    val member: Int by Box(1)\n\
\n\
    companion object {\n\
        val shared: String by Box(\"shared\")\n\
    }\n\
}\n\
\n\
object Single {\n\
    val inObject: Long by Box(2L)\n\
}\n\
\n\
enum class Kind {\n\
    ONE;\n\
\n\
    val label: String by Box(\"one\")\n\
}\n\
";

#[test]
fn a_property_delegate_field_has_no_generic_signature() {
    for class in [
        "store/DelegateFieldSignatureKt",
        "store/Holder",
        "store/Single",
        "store/Kind",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "DelegateFieldSignature",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
