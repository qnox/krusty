//! An unparenthesized safe-call selector includes the following index, assignment, and update.
//!
//! `arg?.alias[i]` indexes the non-null member. `(arg?.alias)[i]` and `(arg?.alias[i])` index or
//! update the nullable result, so an extension on `Foo?` wins over `Foo`'s member operator.
//! Parentheses inside an index, a safe selector nested in an index, and a second safe index on
//! the right-hand side stay inside the outer null check.

use super::common;

fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

#[test]
fn a_parenthesized_safe_call_selects_the_nullable_operator() {
    agrees_with_kotlinc(
        "ParenthesizedSafeCallOperators",
        "var result = \"none\"\n\
         var marks = 0\n\
         fun member(name: String) { result = \"$name: member\" }\n\
         fun extension(name: String) { result = \"$name: extension\" }\n\
         fun mark(n: Int): Int { marks += 1; n }\n\
         fun checkResult(s: String) {\n\
         \x20   if (s != result) throw RuntimeException(\"fail: $s, but $result\")\n\
         }\n\
         class Foo {\n\
         \x20   var alias: Foo = this\n\
         \x20   operator fun get(index: Int): Foo { member(\"get\"); return this }\n\
         \x20   operator fun set(index: Int, arg: Foo) { member(\"set\") }\n\
         \x20   operator fun plusAssign(arg: String) { member(\"plusAssign\") }\n\
         \x20   operator fun inc(): Foo { member(\"inc\"); return this }\n\
         \x20   operator fun invoke(arg: String) { member(\"invoke\") }\n\
         }\n\
         operator fun Foo?.get(index: Int): Foo? { extension(\"get\"); return this }\n\
         operator fun Foo?.set(index: Int, arg: Foo?) { extension(\"set\") }\n\
         operator fun Foo?.plusAssign(arg: String) { extension(\"plusAssign\") }\n\
         operator fun Foo?.inc(): Foo { extension(\"inc\"); return this!! }\n\
         operator fun Foo?.invoke(arg: String) { extension(\"invoke\") }\n\
         class Bar {\n\
         \x20   var slot: Bar = this\n\
         \x20   operator fun get(index: Int): Bar { member(\"get\"); return this }\n\
         \x20   operator fun set(index: Int, value: Bar?) { member(\"set\") }\n\
         }\n\
         fun huh(arg: Foo?) {\n\
         \x20   arg?.alias[42]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   arg?.alias[42] = arg\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   arg?.alias += \"\"\n\
         \x20   checkResult(\"plusAssign: member\")\n\
         \x20   arg?.alias++\n\
         \x20   checkResult(\"inc: member\")\n\
         \x20   ++arg?.alias\n\
         \x20   checkResult(\"inc: member\")\n\
         \x20   arg?.alias(\"\")\n\
         \x20   checkResult(\"invoke: member\")\n\
         \x20   arg?.alias[42] += \"\"\n\
         \x20   checkResult(\"plusAssign: member\")\n\
         \x20   arg?.alias[42]++\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   ++arg?.alias[42]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   (arg?.alias)[42]\n\
         \x20   checkResult(\"get: extension\")\n\
         \x20   (arg?.alias)[42] = arg\n\
         \x20   checkResult(\"set: extension\")\n\
         \x20   (arg?.alias) += \"\"\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias)(\"\")\n\
         \x20   checkResult(\"invoke: extension\")\n\
         \x20   (arg?.alias)[42] += \"\"\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias[42]) += \"\"\n\
         \x20   checkResult(\"plusAssign: extension\")\n\
         \x20   (arg?.alias[42])++\n\
         \x20   checkResult(\"set: extension\")\n\
         \x20   ++(arg?.alias[42])\n\
         \x20   checkResult(\"get: extension\")\n\
         }\n\
         fun nested(left: Bar?, right: Bar?) {\n\
         \x20   left?.slot[(0)]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   marks = 0\n\
         \x20   left?.slot[mark(1)] = right?.slot[mark(2)]\n\
         \x20   checkResult(\"set: member\")\n\
         \x20   if (marks != 2) throw RuntimeException(\"fail marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   val absent: Bar? = null\n\
         \x20   absent?.slot[mark(1)] = right?.slot[mark(2)]\n\
         \x20   if (marks != 0) throw RuntimeException(\"fail null marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   left?.slot[right?.slot[mark(3)]?.let { 4 } ?: 5]\n\
         \x20   checkResult(\"get: member\")\n\
         \x20   if (marks != 1) throw RuntimeException(\"fail index marks $marks\")\n\
         \x20   marks = 0\n\
         \x20   absent?.slot[right?.slot[mark(3)]?.let { 4 } ?: 5]\n\
         \x20   if (marks != 0) throw RuntimeException(\"fail null index marks $marks\")\n\
         }\n\
         fun box(): String {\n\
         \x20   huh(Foo())\n\
         \x20   nested(Bar(), Bar())\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}
