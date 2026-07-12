//! Overloaded `++`/`--` on a local variable whose type has a user `inc`/`dec` MEMBER operator —
//! desugared to `x = x.inc()` (statement / prefix / postfix; postfix yields the captured old value).
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_inc_local_all_forms() {
    const SRC: &str = "class N(val i: Int) { operator fun inc(): N = N(i + 1) }\n\
        fun box(): String {\n\
        \x20 var a = N(1)\n\
        \x20 a++\n\
        \x20 if (a.i != 2) return \"fail stmt: ${a.i}\"\n\
        \x20 val old = a++\n\
        \x20 if (old.i != 2 || a.i != 3) return \"fail postfix: ${old.i} ${a.i}\"\n\
        \x20 val nw = ++a\n\
        \x20 if (nw.i != 4 || a.i != 4) return \"fail prefix: ${nw.i} ${a.i}\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member inc"), "OK");
}

#[test]
fn member_inc_on_field_and_index_statement() {
    // `obj.x++` / `arr[i]++` (statement position) desugar to `... = ....inc()`, so a user `inc`
    // operator works on a member/index target too.
    const SRC: &str = "class N(val i: Int) { operator fun inc(): N = N(i + 1) }\n\
        class Box(var ref: N)\n\
        fun box(): String {\n\
        \x20 val b = Box(N(5))\n\
        \x20 b.ref++\n\
        \x20 b.ref++\n\
        \x20 val a = arrayOf(N(1))\n\
        \x20 a[0]++\n\
        \x20 return if (b.ref.i == 7 && a[0].i == 2) \"OK\" else \"fail ${b.ref.i} ${a[0].i}\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member/index inc"), "OK");
}

#[test]
fn extension_inc_on_nullable_user_class() {
    // A nullable-receiver operator EXTENSION on a MODULE-declared class (`operator fun C?.inc()`) is
    // safe (no builtin collision) and drives `x++` via a static extension call.
    const SRC: &str = "class C(val n: Int)\n\
        operator fun C?.inc(): C? = C((this?.n ?: 0) + 1)\n\
        fun box(): String {\n\
        \x20 var c: C? = C(5)\n\
        \x20 val old = c++\n\
        \x20 return if (old!!.n == 5 && c!!.n == 6) \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("extension inc"), "OK");
}

#[test]
fn member_dec_local() {
    const SRC: &str = "class N(val i: Int) { operator fun dec(): N = N(i - 1) }\n\
        fun box(): String {\n\
        \x20 var a = N(5)\n\
        \x20 a--\n\
        \x20 val old = a--\n\
        \x20 return if (a.i == 3 && old.i == 4) \"OK\" else \"fail ${a.i} ${old.i}\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member dec"), "OK");
}
