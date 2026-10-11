//! An enum constructor's `init` block closes on its `}` line with a `nop` before the `return`
//! that maps back to the class header, as any other class's constructor does.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
var result = \"OK\"\n\
\n\
enum class Plain {\n\
    A;\n\
\n\
    init {\n\
        result = \"plain\"\n\
    }\n\
}\n\
\n\
enum class Valued(val label: String) {\n\
    B(\"b\");\n\
\n\
    init {\n\
        result = label\n\
    }\n\
}\n\
";

#[test]
fn an_enum_init_block_closes_on_its_brace_line() {
    for class in ["store/Plain", "store/Valued"] {
        common::byte_diff_against_kotlinc_cp(
            "EnumInitBlockLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
