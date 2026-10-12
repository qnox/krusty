//! A constant only a removed instruction named, which a later unrewritten method names again,
//! interns where kotlinc's writer reaches it there: a method header's descriptor right after that
//! header's name, and the code's entries right after that method's other entries, ahead of the
//! class attributes.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
val log = StringBuilder()\n\
\n\
fun probe(): String {\n\
    val m = Int.MAX_VALUE / 2\n\
    for (i in m + 4..m + 10 step m) {\n\
        log.append(i)\n\
    }\n\
    return log.toString()\n\
}\n\
";

#[test]
fn a_static_initializer_reinterns_what_a_removed_step_check_named() {
    common::byte_diff_against_kotlinc_cp(
        "RewrittenOrphanPool",
        SOURCE,
        "store/RewrittenOrphanPoolKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/RewrittenOrphanPoolKt differs from kotlinc: {diff}"));
}
