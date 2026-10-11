//! Arguments of an inlined `String.Companion.format`.
//!
//! The companion receiver is loaded only by the parameter null check the inliner deletes, so it
//! is evaluated and popped and takes no local. The format string is stored, and each vararg
//! element is written into the array where the array is filled, after that string.

use super::common;

const SOURCE: &str = "\
fun simple(amount: Int) = String.format(\"%s\", \"\")\n\
fun ntabs(amount: Int) = String.format(\"%1$-${(amount + 1) * 4}s\", \"\")\n\
fun two(a: String, b: String) = String.format(\"%s%s\", a, b)\n\
private inline fun String.tag(extra: String) = this + extra\n\
fun tagged(value: String) = value.tag(\"!\")\n\
private inline fun String.dropMe(extra: String) = extra\n\
fun dropped(value: String) = value.dropMe(\"ok\")\n\
fun kept(value: String) = value.isEmpty()\n\
";

#[test]
fn inlined_string_format_pops_the_unused_companion_and_fills_the_array_in_place() {
    let Some(diff) = common::byte_diff_against_kotlinc_cp(
        "InlineFormatArguments",
        SOURCE,
        "InlineFormatArgumentsKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    diff.expect("inlined String.format arguments match kotlinc");
}
