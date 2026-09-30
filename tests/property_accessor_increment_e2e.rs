//! How many times a property's accessors run for `++p` and `p++`.
//!
//! Kotlin defines `++p` as `p = p.inc()` followed by the value of `p` — a FRESH read — and `p++` as
//! a temporary holding the old value. For a property with a custom getter that difference is
//! observable: the prefix form calls the getter twice and the postfix form once, whether or not the
//! result is used. kotlinc emits the prefix re-read even in statement position, so a program that
//! counts accessor calls can tell the two apart (`codegen/box/intrinsics/prefixIncDec.kt`,
//! `codegen/box/statics/incInObject.kt`, `codegen/box/statics/incInClassObject.kt`).

use super::common;

/// The same program under the provisioned REFERENCE compiler.
///
/// Accessor-call COUNT is a claim about what kotlinc does, so setup, compilation, emitted `box`,
/// and execution are all mandatory assertions rather than optional coverage.
fn reference_box(src: &str) -> String {
    let out = common::kotlinc_library(src).expect("compile accessor fixture with kotlinc");
    common::run_box(&[], "LibKt", &[out, common::stdlib_jar()])
        .expect("run kotlinc-built accessor fixture")
}

/// Run `src` under both compilers and assert both answer `"OK"`.
fn both_compilers_agree(src: &str, stem: &str) {
    common::expect_box_ok_with_stdlib(src, stem);
    assert_eq!(reference_box(src), "OK", "{stem}: reference compiler");
}

#[test]
fn a_prefix_increment_reads_the_property_again_and_a_postfix_one_does_not() {
    both_compilers_agree(
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

/// The prefix re-read is the SAME selected access as the first read, context arguments included.
///
/// A property with context parameters takes them as leading getter operands. Rebuilding the second
/// read from scratch dropped them, and the emitted getter call was left one operand short — the
/// front end accepted the program and the backend bailed on it. Both reads now come from one
/// builder, so there is no second spelling to leave anything off.
#[test]
fn a_prefix_increment_of_a_context_property_re_reads_with_its_context_argument() {
    both_compilers_agree(
        "class Ctx(val tag: String)\n\
         var log = \"\"\n\
         var store = 0\n\
         context(c: Ctx) var counted: Int\n\
         \x20   get() { log += \"g\" + c.tag; return store }\n\
         \x20   set(v) { log += \"s\" + c.tag; store = v }\n\
         \n\
         fun box(): String {\n\
         \x20   with(Ctx(\"X\")) { ++counted }\n\
         \x20   if (log != \"gXsXgX\") return \"fail: prefix statement ran $log\"\n\
         \x20   log = \"\"\n\
         \x20   with(Ctx(\"Y\")) { counted++ }\n\
         \x20   if (log != \"gYsY\") return \"fail: postfix statement ran $log\"\n\
         \x20   log = \"\"\n\
         \x20   val value = with(Ctx(\"Z\")) { ++counted }\n\
         \x20   if (log != \"gZsZgZ\" || value != 3) return \"fail: prefix value ran $log -> $value\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "PropIncContextArgs",
    );
}

/// A qualified or indexed prefix increment re-reads after the write. The receiver and the index
/// operands are evaluated once; the getter or `get` runs again for the prefix result, including
/// when that result is discarded. Postfix keeps the first read.
#[test]
fn a_prefix_increment_of_a_qualified_or_indexed_access_rereads() {
    both_compilers_agree(
        "var log = \"\"\n\
         fun <T> logged(value: T): T { log += \"$value;\"; return value }\n\
         object A {\n\
         \x20   var x = 0\n\
         \x20       get() { log += \"get;\"; return field }\n\
         \x20       set(value: Int) { log += \"set;\"; field = value }\n\
         }\n\
         fun getA(): A { log += \"recv;\"; return A }\n\
         object B {\n\
         \x20   var x = 0\n\
         \x20   operator fun get(i1: Int, i2: Int): Int { log += \"get($i1,$i2);\"; return x }\n\
         \x20   operator fun set(i1: Int, i2: Int, value: Int) {\n\
         \x20       log += \"set($i1,$i2,$value);\"; x = value\n\
         \x20   }\n\
         }\n\
         fun getB(): B { log += \"recvB;\"; return B }\n\
         fun box(): String {\n\
         \x20   ++getA().x\n\
         \x20   if (log != \"recv;get;set;get;\") return \"fail stmt prefix $log\"\n\
         \x20   log = \"\"\n\
         \x20   getA().x--\n\
         \x20   if (log != \"recv;get;set;\") return \"fail stmt postfix $log\"\n\
         \x20   log = \"\"\n\
         \x20   val prefixValue = ++getA().x\n\
         \x20   if (log != \"recv;get;set;get;\" || prefixValue != 1) return \"fail value prefix $log -> $prefixValue\"\n\
         \x20   log = \"\"\n\
         \x20   val postfixValue = getA().x--\n\
         \x20   if (log != \"recv;get;set;\" || postfixValue != 1) return \"fail value postfix $log -> $postfixValue\"\n\
         \x20   log = \"\"\n\
         \x20   val indexed = ++getB()[logged(1), logged(2)]\n\
         \x20   if (log != \"recvB;1;2;get(1,2);set(1,2,1);get(1,2);\" || indexed != 1) {\n\
         \x20       return \"fail index prefix $log -> $indexed\"\n\
         \x20   }\n\
         \x20   log = \"\"\n\
         \x20   val indexedPost = getB()[logged(3), logged(4)]--\n\
         \x20   if (log != \"recvB;3;4;get(3,4);set(3,4,0);\" || indexedPost != 1) {\n\
         \x20       return \"fail index postfix $log -> $indexedPost\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n",
        "QualifiedPrefixReread",
    );
}
