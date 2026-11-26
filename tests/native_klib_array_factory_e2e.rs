//! `kotlin.arrayOf` reached from a KLIB body: the array its one vararg argument packs.
//!
//! The Native stdlib's string case mapping builds a reference array with `arrayOf`, so the program
//! below runs only when that factory lowers from the library body.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn a_klib_body_packs_its_arrayof_arguments() {
    let source = "fun box(): String = \"ok\".uppercase()\n";
    expect_box_ok_with_stdlib(source, "KlibBodyArrayOf");
    expect_native_box(source, "KlibBodyArrayOf", "OK");
}
