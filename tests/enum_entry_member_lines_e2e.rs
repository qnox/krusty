//! Methods and properties declared in an enum entry's class body carry their source lines, as a
//! class member's do.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
fun mark(v: Int) = v\n\
\n\
enum class Shade {\n\
    LIGHT {\n\
        fun empty() {}\n\
\n\
        override fun level(): Int {\n\
            mark(1)\n\
            return 2\n\
        }\n\
\n\
        val depth: Int\n\
            get() =\n\
                mark(3)\n\
    },\n\
    DARK;\n\
\n\
    open fun level(): Int = 0\n\
}\n\
";

#[test]
fn enum_entry_members_keep_their_lines() {
    for class in ["store/Shade$LIGHT", "store/Shade"] {
        common::byte_diff_against_kotlinc_cp(
            "EnumEntryMemberLines",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
