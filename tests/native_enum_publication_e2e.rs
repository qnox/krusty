//! When Kotlin/Native publishes an enum's constants, observed from inside their constructors.
//!
//! kotlinc-native's `EnumClassLowering` allocates every constant first (`VALUES`, an array of
//! `createUninitializedInstance` in name order) and only then runs each constant's constructor on
//! its allocation, in declaration order. So a constructor that reaches back into its own enum
//! finds every constant: the ones not constructed yet with zero fields and `ordinal` 0. `values()`
//! is `valuesForEnum`, which stores each constant at its CURRENT ordinal while walking `VALUES`,
//! so the not-yet-constructed constants collapse onto index 0 and leave their own places `null`.
//! `entries` is built after every constant, so it is still `null` in there.
//!
//! The expectation is kotlinc-native's, taken by running the same program under it. The program
//! never reads the `name` of a constant that is not constructed yet: kotlinc-native leaves it
//! unset, and both `toString()` and `valueOf` (a binary search over the names) crash on it.

use super::common::{expect_native_box, kotlin_native_box};

const BACK_EDGES: &str = "var log = \"\"\n\
fun describe(x: Any?, self: Any): String = when {\n\
\x20   x == null -> \"null\"\n\
\x20   x === self -> \"self\"\n\
\x20   else -> \"other\"\n\
}\n\
fun seen(self: Any): String {\n\
\x20   val values = E.values()\n\
\x20   var order = \"\"\n\
\x20   var ordinals = \"\"\n\
\x20   for (i in 0 until values.size) {\n\
\x20       if (i > 0) {\n\
\x20           order += \",\"\n\
\x20           ordinals += \",\"\n\
\x20       }\n\
\x20       val value = values[i] as E?\n\
\x20       order += describe(value, self)\n\
\x20       ordinals += if (value == null) \"null\" else value.ordinal.toString()\n\
\x20   }\n\
\x20   val entries = if ((E.entries as Any?) == null) \"null\" else \"list\"\n\
\x20   return \"[$order|$ordinals|${describe(E.C, self)}${describe(E.A, self)}${describe(E.B, self)}|${E.A.tag}|$entries]\"\n\
}\n\
enum class E(val tag: Int) {\n\
\x20   C(3),\n\
\x20   A(1),\n\
\x20   B(2) {\n\
\x20       init { log += \"body\" + seen(this) }\n\
\x20   };\n\
\x20   init { log += ordinal.toString() + seen(this) }\n\
}\n\
fun box(): String {\n\
\x20   E.A\n\
\x20   var names = \"\"\n\
\x20   val values = E.values()\n\
\x20   for (i in 0 until values.size) names += (if (i > 0) \", \" else \"\") + values[i].name\n\
\x20   val missing = try { E.valueOf(\"D\").name } catch (e: IllegalArgumentException) { e.message }\n\
\x20   return log + \"#\" + names + \"#\" + missing\n\
}\n";

/// What kotlinc-native prints for [`BACK_EDGES`].
const PUBLISHED_FIRST: &str = "\
0[self,null,null|0,null,null|selfotherother|0|null]\
1[other,self,null|0,1,null|otherselfother|1|null]\
2[other,other,self|0,1,2|otherotherself|1|null]\
body[other,other,self|0,1,2|otherotherself|1|null]\
#C, A, B#Invalid enum value name: D";

/// Every constant is allocated and published before the first constructor runs, and each is then
/// constructed in place in declaration order.
#[test]
fn constants_are_published_before_their_constructors_run() {
    if let Some(answer) = kotlin_native_box(BACK_EDGES) {
        assert_eq!(answer, PUBLISHED_FIRST, "kotlinc-native's answer");
    }
    expect_native_box(BACK_EDGES, "EnumPublication", PUBLISHED_FIRST);
}
