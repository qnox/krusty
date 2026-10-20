//! kotlinc's parser treats a line break inside `(…)` and `[…]` as whitespace and restores it as a
//! statement terminator in every `{…}` body nested inside.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_line_break_inside_parentheses_continues_the_expression() {
    let source = "fun pair(a: Int, b: Int) = a * 10 + b\n\
         \n\
         fun mixed(a: Int, b: Int) = pair(\n\
         \x20   a\n\
         \x20   or b,\n\
         \x20   1)\n\
         \n\
         fun box(): String {\n\
         \x20   val text = (\n\
         \x20       \"x\"\n\
         \x20       + 1\n\
         \x20       + \"y\")\n\
         \x20   val bits = pair(\n\
         \x20       1\n\
         \x20       or 2,\n\
         \x20       4\n\
         \x20       - 1)\n\
         \x20   val list = listOf(5, 6, 7)\n\
         \x20   val item = list[\n\
         \x20       2\n\
         \x20       - 1]\n\
         \x20   if (text != \"x1y\") return text\n\
         \x20   if (bits != 33) return \"bits $bits\"\n\
         \x20   if (mixed(1, 2) != 31) return \"mixed\"\n\
         \x20   return if (item == 6) \"OK\" else \"item $item\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "ParenthesizedLineBreak");
    assert_module_matches_kotlinc(&[("ParenthesizedLineBreak.kt", source)]);
}

#[test]
fn a_block_inside_parentheses_keeps_its_line_breaks_as_terminators() {
    let source = "fun box(): String {\n\
         \x20   val value = run({\n\
         \x20       val a = 1\n\
         \x20       -a\n\
         \x20   })\n\
         \x20   val chosen = maxOf(\n\
         \x20       when {\n\
         \x20           value < 0 -> 2\n\
         \x20           else -> 3\n\
         \x20       },\n\
         \x20       1)\n\
         \x20   return if (value == -1 && chosen == 2) \"OK\" else \"$value $chosen\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "BlockInParentheses");
    assert_module_matches_kotlinc(&[("BlockInParentheses.kt", source)]);
}
