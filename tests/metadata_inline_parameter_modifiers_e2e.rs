//! The inline modifiers a value parameter wrote, as `ValueParameter.flags` records them against
//! kotlinc 2.4.20: `crossinline` is bit 2 and `noinline` bit 3, beside `DECLARES_DEFAULT_VALUE`
//! (bit 1), on a top-level function, an extension and a class member alike.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const SRC: &str = "package app\n\
    \n\
    inline fun top(crossinline a: () -> Unit, noinline b: () -> Unit, c: () -> Unit): Int {\n\
    \x20   c()\n\
    \x20   return 1\n\
    }\n\
    \n\
    inline fun String.ext(noinline b: (String) -> Int, crossinline n: () -> Unit = {}): Int {\n\
    \x20   n()\n\
    \x20   return b(this)\n\
    }\n\
    \n\
    inline fun nullable(noinline b: (() -> Unit)?) {}\n\
    \n\
    class Host {\n\
    \x20   inline fun member(crossinline a: () -> Unit, noinline b: () -> Unit) {\n\
    \x20       b()\n\
    \x20   }\n\
    \x20   inline fun Int.memberExt(noinline b: (Int) -> Int, c: () -> Unit): Int = b(this)\n\
    }\n";

#[test]
fn package_function_parameters_record_their_inline_modifiers() {
    assert_identical("InlineModifiers", SRC, "app/InlineModifiersKt");
}

#[test]
fn member_parameters_record_their_inline_modifiers() {
    assert_identical("InlineModifierMembers", SRC, "app/Host");
}
