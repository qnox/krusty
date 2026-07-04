//! Unsigned primitive arrays (`UIntArray`/`ULongArray`): the factory (`uintArrayOf`/`ulongArrayOf`),
//! the size constructors (bare `UIntArray(n)` and the `UIntArray(n) { i -> e }` fill form), and element
//! read/write on the unboxed underlying primitive array (`[I`/`[J`). Round-tripped on the JVM under
//! `-Xverify:all` via `compile_and_run_box`.

use super::common;

fn run_ok(stem: &str, body: &str) {
    let src = format!("fun box(): String {{\n{body}\n}}\n");
    common::expect_box_ok_with_stdlib(&src, stem);
}

#[test]
fn uint_array_of_literal() {
    run_ok(
        "UIntOf",
        "val a = uintArrayOf(1u, 2u, 3u)\nreturn if (a[1] == 2u && a.size == 3) \"OK\" else \"F\"",
    );
}

#[test]
fn uint_array_bare_ctor_and_store() {
    run_ok(
        "UIntBare",
        "val a = UIntArray(3)\na[0] = 5u; a[1] = 7u\nreturn if (a[0] + a[1] == 12u && a[2] == 0u) \"OK\" else \"F\"",
    );
}

#[test]
fn uint_array_fill_ctor() {
    run_ok(
        "UIntFill",
        "val a = UIntArray(4) { (it * 2).toUInt() }\nreturn if (a[3] == 6u && a[0] == 0u) \"OK\" else \"F a3=${a[3]}\"",
    );
}

#[test]
fn uint_array_iterate_sum() {
    run_ok(
        "UIntIter",
        "val a = uintArrayOf(10u, 20u, 30u)\nvar s = 0u\nfor (x in a) s += x\nreturn if (s == 60u) \"OK\" else \"F s=$s\"",
    );
}

#[test]
fn uint_array_index_loop() {
    run_ok(
        "UIntIdx",
        "val a = uintArrayOf(2u, 4u, 6u)\nvar s = 0u\nfor (i in a.indices) s += a[i]\nreturn if (s == 12u) \"OK\" else \"F\"",
    );
}

#[test]
fn ulong_array_of_literal() {
    run_ok(
        "ULongOf",
        "val a = ulongArrayOf(1uL, 2uL)\nreturn if (a[0] == 1uL && a[1] == 2uL) \"OK\" else \"F\"",
    );
}

#[test]
fn ulong_array_bare_and_fill() {
    run_ok(
        "ULongMix",
        "val a = ULongArray(2)\na[0] = 9uL\nval b = ULongArray(3) { (it.toLong() + 1L).toULong() }\n\
         return if (a[0] == 9uL && a[1] == 0uL && b[2] == 3uL) \"OK\" else \"F\"",
    );
}
