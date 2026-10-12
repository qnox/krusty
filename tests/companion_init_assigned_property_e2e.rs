//! A companion property with no initializer, assigned only by the companion's `init` blocks, is
//! stored like any other companion property: a private static of the outer class, reached through
//! `access$get…$cp`/`access$set…$cp`. The `init` block runs in the outer `<clinit>`, writing the
//! static in source order.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Assigned {\n\
    companion object {\n\
        val label: String\n\
        var count: Int\n\
        var narrowed: Int\n\
            private set\n\
        private val secret: String\n\
\n\
        init {\n\
            label = \"a\"\n\
            count = 2\n\
            narrowed = 3\n\
            secret = \"s\"\n\
        }\n\
\n\
        fun show() = secret\n\
    }\n\
}\n\
\n\
class Mixed {\n\
    companion object {\n\
        val first = 1\n\
        val second: Int\n\
\n\
        init {\n\
            second = first + 1\n\
        }\n\
    }\n\
}\n\
\n\
class Custom {\n\
    companion object {\n\
        private val decorated: String\n\
            get() = field + \"!\"\n\
\n\
        init {\n\
            decorated = \"OK\"\n\
        }\n\
\n\
        fun shown(): String = ::decorated.get()\n\
    }\n\
}\n\
\n\
fun box(): String = Custom.shown()\n\
";

const CUSTOM_SETTER_SOURCE: &str = "\
class CustomSetter {\n\
    companion object {\n\
        private var adjusted: Int\n\
            set(value) { field = value + 1 }\n\
\n\
        init { adjusted = 0 }\n\
\n\
        fun throughReference(): Int {\n\
            ::adjusted.set(40)\n\
            return ::adjusted.get()\n\
        }\n\
    }\n\
}\n\
\n\
fun box(): String = if (CustomSetter.throughReference() == 41) \"OK\" else \"FAIL\"\n\
";

const RETAINED_COMPANION_INITIALIZER_SOURCE: &str = "\
package store\n\
\n\
class RetainedInitializer {\n\
    companion object {\n\
        var count = 1\n\
        lateinit var text: String\n\
\n\
        init {\n\
            count = 2\n\
            text = \"OK\"\n\
        }\n\
\n\
        fun shown(): String = if (count == 2) text else \"fail:$count\"\n\
    }\n\
}\n\
\n\
fun box(): String = RetainedInitializer.shown()\n\
";

#[test]
fn an_init_assigned_companion_property_is_an_outer_static() {
    for class in [
        "store/Assigned",
        "store/Assigned$Companion",
        "store/Mixed",
        "store/Mixed$Companion",
        "store/Custom",
        "store/Custom$Companion",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionInitAssignedProperty",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn a_reference_to_a_hoisted_property_keeps_its_declared_getter() {
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "CompanionInitAssignedProperty").as_deref(),
        Some("OK!")
    );
}

#[test]
fn a_reference_to_a_hoisted_property_keeps_its_declared_setter() {
    assert_eq!(
        common::compile_and_run_with_stdlib(CUSTOM_SETTER_SOURCE, "CompanionCustomSetterReference")
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn a_non_hoisted_field_keeps_the_companion_initializer_in_source_order() {
    for class in [
        "store/RetainedInitializer",
        "store/RetainedInitializer$Companion",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "RetainedCompanionInitializer",
            RETAINED_COMPANION_INITIALIZER_SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    assert_eq!(
        common::compile_and_run_with_stdlib(
            RETAINED_COMPANION_INITIALIZER_SOURCE,
            "RetainedCompanionInitializer",
        )
        .as_deref(),
        Some("OK")
    );
}
