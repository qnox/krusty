//! A bound property reference's `get` over static storage does not load its `receiver`: a static
//! field, `access$get…$cp()` and `access$get…$p()` take none, and kotlinc's field read drops the
//! receiver. `set` still evaluates it (`getfield receiver; pop`).

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Meter {\n\
    companion object {\n\
        private var reading = \"a\"\n\
        private val scale = 1\n\
        @JvmField var shared = \"s\"\n\
        fun readingRef() = this::reading\n\
        fun scaleRef() = Meter.Companion::scale\n\
    }\n\
    fun outerRef() = Companion::reading\n\
}\n\
\n\
object Board {\n\
    @JvmField var note = \"n\"\n\
}\n\
\n\
fun sharedRef() = Meter.Companion::shared\n\
fun noteRef() = Board::note\n\
";

#[test]
fn a_bound_static_property_reference_get_reads_no_receiver() {
    for class in [
        "store/Meter$Companion$readingRef$1",
        "store/Meter$Companion$scaleRef$1",
        "store/Meter$outerRef$1",
        "store/BoundStaticPropertyReferenceGetKt$sharedRef$1",
        "store/BoundStaticPropertyReferenceGetKt$noteRef$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "BoundStaticPropertyReferenceGet",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
