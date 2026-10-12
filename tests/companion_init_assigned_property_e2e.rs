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
        private val secret: String\n\
\n\
        init {\n\
            label = \"a\"\n\
            count = 2\n\
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
";

#[test]
fn an_init_assigned_companion_property_is_an_outer_static() {
    for class in [
        "store/Assigned",
        "store/Assigned$Companion",
        "store/Mixed",
        "store/Mixed$Companion",
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
