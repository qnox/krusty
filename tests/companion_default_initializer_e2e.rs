//! A hoisted companion property whose initializer is its field's JVM default (`0`, `null`) gets no
//! `<clinit>` store, as kotlinc's `visitSetField` drops a field initializer that only repeats the
//! default value. Any other initializer is stored.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Pub {\n\
    companion object {\n\
        var counter: Int = 0\n\
        var other: Int = 5\n\
        var hidden: Int = 0\n\
        val fixed: Int = 0\n\
        var note: String? = null\n\
        fun bump() { hidden = hidden + 1 }\n\
    }\n\
}\n\
";

#[test]
fn a_companion_property_initialized_to_its_default_has_no_store() {
    for class in ["store/Pub", "store/Pub$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionDefaultInitializer",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
