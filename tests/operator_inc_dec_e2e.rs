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
