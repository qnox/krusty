//! A private `lateinit` companion property is stored on the outer class like every other private
//! companion property. It has no accessor, so the outer class reads the static with the
//! uninitialized guard inline, the companion and other classes read it through
//! `access$get…$cp` followed by the same guard, and `isInitialized` loads the raw field.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Registry {\n\
    fun current(): String = active\n\
    fun install(name: String) {\n\
        active = name\n\
    }\n\
    fun ready(): Boolean = Companion.ready()\n\
\n\
    companion object {\n\
        private lateinit var active: String\n\
        fun touch() {\n\
            active\n\
        }\n\
        fun ready(): Boolean = ::active.isInitialized\n\
        fun label(): String = active + \"!\"\n\
    }\n\
\n\
    class Probe {\n\
        fun peek(): String = active\n\
    }\n\
}\n\
";

#[test]
fn a_private_lateinit_companion_property_lives_on_the_outer_class() {
    for class in [
        "store/Registry",
        "store/Registry$Companion",
        "store/Registry$Probe",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "LateinitCompanionHoist",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
