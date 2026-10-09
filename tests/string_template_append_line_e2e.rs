//! A multi-part string concatenation marks each part on its own line, then marks `append` on the
//! concatenation's line. Neighbouring constants keep the first constant's line, and a one-unit
//! literal does not open a line of its own.

use super::common;

const SOURCE: &str = "\
fun seq(a: Any, b: Any): String {\n\
    return \"x $a \" +\n\
        \"y $b\"\n\
}\n\
\n\
fun first(a: Any): String {\n\
    return \"x $a \" +\n\
        \"y\"\n\
}\n\
\n\
fun mid(a: Any, b: Any): String {\n\
    return \"x \" +\n\
        \"$a y \" +\n\
        \"$b\"\n\
}\n\
\n\
fun same(a: Any, b: Any) = \"x $a y $b\"\n\
\n\
fun call(a: Any): String {\n\
    return foo(\n\
        \"x \" +\n\
            \"y $a\"\n\
    )\n\
}\n\
\n\
fun foo(s: String) = s\n\
\n\
fun bang(a: Any): String {\n\
    return \"x $a\" +\n\
        \"!\"\n\
}\n\
\n\
fun piece(a: Any): String {\n\
    return \"x \" +\n\
        \"$a!\"\n\
}\n\
";

#[test]
fn string_template_append_lines_match_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "StringTemplateAppendLine",
        SOURCE,
        "StringTemplateAppendLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("string template append lines differ from kotlinc: {diff}"));
}
