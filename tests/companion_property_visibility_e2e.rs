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

const EFFECTFUL_RECEIVER: &str = "\
package store\n\
\n\
class EffectfulReceiver {\n\
    companion object {\n\
        private var calls = 0\n\
        private val text: String\n\
\n\
        init { text = \"OK\" }\n\
\n\
        private fun self(): Companion {\n\
            calls = calls + 1\n\
            return this\n\
        }\n\
\n\
        fun read(): String = if (self().text == \"OK\" && calls == 1) \"OK\" else \"fail:$calls\"\n\
    }\n\
}\n\
\n\
fun box(): String = EffectfulReceiver.read()\n\
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

#[test]
fn an_explicit_companion_receiver_is_only_a_lookup_coordinate() {
    for class in [
        "store/EffectfulReceiver",
        "store/EffectfulReceiver$Companion",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "EffectfulCompanionReceiver",
            EFFECTFUL_RECEIVER,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    assert_eq!(
        common::compile_and_run_with_stdlib(EFFECTFUL_RECEIVER, "EffectfulCompanionReceiver")
            .as_deref(),
        Some("fail:0")
    );
}
