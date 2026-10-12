//! A protected or internal companion property is stored like a public or private one: a private
//! static of the outer class, reached through `access$get…$cp`/`access$set…$cp`. Its companion
//! accessors keep the property's visibility: a protected getter and setter, and an internal pair
//! with the module-mangled `getX$main` name.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
open class Vis {\n\
    companion object {\n\
        internal val inner: String = \"i\"\n\
        protected val guarded: String = \"g\"\n\
        protected var counter: Int = 0\n\
        internal var mutable: Long = 1L\n\
    }\n\
\n\
    inner class Reader {\n\
        fun read() = guarded + inner + counter + mutable\n\
    }\n\
}\n\
";

#[test]
fn an_internal_or_protected_companion_property_is_an_outer_static() {
    for class in ["store/Vis", "store/Vis$Companion", "store/Vis$Reader"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionPropertyVisibility",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
