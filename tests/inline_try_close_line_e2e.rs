//! An inlined lambda marks a returned `try`'s closing `}` on the path that evaluated the `try`.
//! `return@` jumps to the join after that `nop`. A returned name does not grow a `nop` on the
//! lambda's own `}`.

use super::common;

const SOURCE: &str = "\
inline fun <R> choose(transform: () -> R): R = transform()\n\
\n\
fun pick(item: String): String? {\n\
    return choose choice@ {\n\
        if (item.isEmpty()) return@choice null\n\
        item\n\
    }\n\
}\n\
\n\
fun pickElse(item: String): String? =\n\
    choose {\n\
        if (item.isEmpty()) null else item\n\
    }\n\
\n\
fun branched(item: String): String? {\n\
    return choose choice@ {\n\
        if (item.isEmpty()) return@choice null\n\
        try {\n\
            item\n\
        } catch (e: Exception) {\n\
            null\n\
        }\n\
    }\n\
}\n\
\n\
fun onlyTry(item: String): String? {\n\
    return choose {\n\
        try {\n\
            item\n\
        } catch (e: Exception) {\n\
            null\n\
        }\n\
    }\n\
}\n\
";

#[test]
fn an_inlined_try_close_matches_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "InlineTryCloseLine",
        SOURCE,
        "InlineTryCloseLineKt",
        &[common::stdlib_jar(), common::jdk_modules()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("inlined try close differs from kotlinc: {diff}"));
}
