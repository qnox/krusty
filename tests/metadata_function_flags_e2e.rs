//! Declaration modifiers `@Metadata` carries in `Function.flags`, measured against kotlinc 2.4.20:
//! `tailrec` is bit 11, on a top-level function, a class member and an anonymous object's member
//! alike.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const TAILREC_SRC: &str = "package app\n\
    \n\
    tailrec fun down(n: Int): Int = if (n == 0) 0 else down(n - 1)\n\
    \n\
    class Counter {\n\
    \x20   tailrec fun loop(n: Int): Int = if (n == 0) 0 else loop(n - 1)\n\
    \x20   fun plain(): Int = 1\n\
    }\n\
    \n\
    fun make(): Any = object {\n\
    \x20   tailrec fun inner(n: Int): Int = if (n == 0) 0 else inner(n - 1)\n\
    }\n";

#[test]
fn a_top_level_tailrec_function_records_tailrec() {
    assert_identical("TailrecFlags", TAILREC_SRC, "app/TailrecFlagsKt");
}

#[test]
fn a_tailrec_member_records_tailrec() {
    assert_identical("TailrecMember", TAILREC_SRC, "app/Counter");
}

#[test]
fn an_anonymous_object_tailrec_member_records_tailrec() {
    assert_identical("TailrecObject", TAILREC_SRC, "app/TailrecObjectKt$make$1");
}
