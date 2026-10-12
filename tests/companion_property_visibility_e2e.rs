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
\n\
class InitializerBacked {\n\
    class Nested {\n\
        fun update(): String {\n\
            text = \"OK\"\n\
            return text\n\
        }\n\
    }\n\
\n\
    companion object {\n\
        var text = \"fail\"\n\
            private set\n\
    }\n\
}\n\
\n\
fun box(): String = InitializerBacked.Nested().update()\n\
";

#[test]
fn an_internal_or_protected_companion_property_is_an_outer_static() {
    for class in [
        "store/Vis",
        "store/Vis$Companion",
        "store/Vis$Reader",
        "store/InitializerBacked",
        "store/InitializerBacked$Companion",
        "store/InitializerBacked$Nested",
    ] {
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

#[test]
fn an_initializer_backed_private_setter_keeps_the_nested_class_bridge() {
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "CompanionPropertyVisibility").as_deref(),
        Some("OK")
    );
}
