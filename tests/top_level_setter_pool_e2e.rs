//! A top-level property's default setter interns its `<set-?>` local-variable name after its
//! body, as kotlinc's writer does, so the facade's constant pool keeps kotlinc's order.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
var flag = true\n\
\n\
var count = 0L\n\
\n\
fun probe(): Int {\n\
    if (flag) return 1\n\
    return 2\n\
}\n\
";

#[test]
fn top_level_setter_name_keeps_kotlinc_pool_order() {
    common::byte_diff_against_kotlinc_cp(
        "TopLevelSetterPool",
        SOURCE,
        "store/TopLevelSetterPoolKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("top-level setter pool differs from kotlinc: {diff}"));
}
