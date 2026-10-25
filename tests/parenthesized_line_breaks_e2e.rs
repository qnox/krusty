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

#[test]
fn a_when_subject_and_an_object_header_inside_parentheses_may_start_on_the_next_line() {
    let source = "open class Base(val n: Int)\n\
         \n\
         fun id(base: Base) = base.n\n\
         \n\
         fun sum(a: Int, b: Int) = a + b\n\
         \n\
         fun choose(key: Int) = sum(when\n\
         \x20   (key) {\n\
         \x20       1 -> 5\n\
         \x20       else -> 2\n\
         \x20   }, 0)\n\
         \n\
         fun build() = id(object : Base\n\
         \x20   (7) {\n\
         \x20       val extra = n\n\
         \x20       fun twice() = extra * 2\n\
         \x20   })\n\
         \n\
         fun box(): String {\n\
         \x20   val chosen = choose(1)\n\
         \x20   val built = build()\n\
         \x20   return if (chosen == 5 && built == 7) \"OK\" else \"$chosen $built\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "BodyBoundaryInParentheses");
    assert_module_matches_kotlinc(&[("BodyBoundaryInParentheses.kt", source)]);
}

#[test]
fn every_value_argument_list_continues_across_a_line_break() {
    let source = "annotation class Tag(val value: Int)\n\
         \n\
         open class Base(val n: Int)\n\
         \n\
         enum class Level(val weight: Int) {\n\
         \x20   LOW(1\n\
         \x20       + 1),\n\
         \x20   HIGH(10\n\
         \x20       * 2),\n\
         }\n\
         \n\
         class Derived : Base(3\n\
         \x20   + 4)\n\
         \n\
         class Point(val x: Int) {\n\
         \x20   constructor(a: Int, b: Int) : this(a\n\
         \x20       - b)\n\
         }\n\
         \n\
         class Box(val size: Int) {\n\
         \x20   fun grow(by: Int) = size + by\n\
         }\n\
         \n\
         @Tag(1\n\
         \x20   + 2)\n\
         fun tagged() = 1\n\
         \n\
         fun boxed(box: Box?): Int? {\n\
         \x20   return box?.grow(1\n\
         \x20       + 2)\n\
         }\n\
         \n\
         fun box(): String {\n\
         \x20   val levels = Level.LOW.weight + Level.HIGH.weight\n\
         \x20   val derived = Derived().n\n\
         \x20   val point = Point(9, 2).x\n\
         \x20   val grown = boxed(Box(5))!!\n\
         \x20   val sum = levels + derived + point + grown + tagged()\n\
         \x20   return if (sum == 22 + 7 + 7 + 8 + 1) \"OK\" else \"sum $sum\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "ArgumentListLineBreaks");
    assert_module_matches_kotlinc(&[("ArgumentListLineBreaks.kt", source)]);
}
