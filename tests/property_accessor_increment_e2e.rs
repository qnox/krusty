//! How many times a property's accessors run for `++p` and `p++`.
//!
//! Kotlin defines `++p` as `p = p.inc()` followed by the value of `p` — a FRESH read — and `p++` as
//! a temporary holding the old value. For a property with a custom getter that difference is
//! observable: the prefix form calls the getter twice and the postfix form once, whether or not the
//! result is used. kotlinc emits the prefix re-read even in statement position, so a program that
//! counts accessor calls can tell the two apart (`codegen/box/intrinsics/prefixIncDec.kt`,
//! `codegen/box/statics/incInObject.kt`, `codegen/box/statics/incInClassObject.kt`).

use super::common;

#[test]
fn a_prefix_increment_reads_the_property_again_and_a_postfix_one_does_not() {
    common::expect_box_ok_with_stdlib(
        "var log = \"\"\n\
         var topLevel: Int = 0\n\
         \x20   get() { log += \"g\"; return field }\n\
         \x20   set(v) { log += \"s\"; field = v }\n\
         \n\
         object Holder {\n\
         \x20   var seen = \"\"\n\
         \x20   var counted: Int = 0\n\
         \x20       get() { seen += \"g\"; return field }\n\
         \x20   fun bump(): String {\n\
         \x20       counted++\n\
         \x20       ++counted\n\
         \x20       return seen\n\
         \x20   }\n\
         }\n\
         \n\
         fun box(): String {\n\
         \x20   ++topLevel\n\
         \x20   if (log != \"gsg\") return \"fail: prefix statement ran $log\"\n\
         \x20   log = \"\"\n\
         \x20   topLevel++\n\
         \x20   if (log != \"gs\") return \"fail: postfix statement ran $log\"\n\
         \x20   log = \"\"\n\
         \x20   val prefixValue = ++topLevel\n\
         \x20   if (log != \"gsg\" || prefixValue != 3) return \"fail: prefix value ran $log -> $prefixValue\"\n\
         \x20   log = \"\"\n\
         \x20   val postfixValue = topLevel++\n\
         \x20   if (log != \"gs\" || postfixValue != 3) return \"fail: postfix value ran $log -> $postfixValue\"\n\
         \x20   val seen = Holder.bump()\n\
         \x20   if (seen != \"ggg\") return \"fail: getter-only ran $seen\"\n\
         \x20   if (Holder.counted != 2) return \"fail: getter-only value ${Holder.counted}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "PropIncAccessors",
    );
}
