//! A local whose range holds no instruction, as kotlinc's optimizer treats it: the entry stays in
//! the method through every pass, so the store in front of it is a named local's and not a
//! temporary's, and it goes only when the method is written (`prepareForEmitting`).
//!
//! krusty dropped such an entry when the method was added, so the temporaries pass took the store
//! for a temporary nothing loads and removed it (`nop` for `iconst_5; istore_1`).
use super::common;

/// Compile `src` with kotlinc and krusty and require `class` to be the same bytes.
fn byte_identical(name: &str, src: &str, class: &str) {
    common::byte_diff_against_kotlinc(name, src, class)
        .expect("the reference kotlinc is required")
        .unwrap_or_else(|difference| panic!("{difference}"));
}

/// A `val` that is the last statement of its block: its range opens after its store and closes at
/// the block's end. An `Int`, a reference and a `Long` (whose store takes two slots).
#[test]
fn a_block_ending_in_an_unused_val_keeps_its_store() {
    byte_identical(
        "EmptyLocal",
        "fun store(x: Boolean): Int {\n\
         \x20   if (x) {\n\
         \x20       val unused = 5\n\
         \x20   }\n\
         \x20   return 1\n\
         }\n\
         fun storeRef(x: Boolean, s: String): String {\n\
         \x20   if (x) {\n\
         \x20       val alias = s\n\
         \x20   }\n\
         \x20   return s\n\
         }\n\
         fun storeLong(n: Int): Long {\n\
         \x20   var total = 0L\n\
         \x20   if (n > 0) {\n\
         \x20       val wide = total + n\n\
         \x20   }\n\
         \x20   return total\n\
         }\n",
        "EmptyLocalKt",
    );
}

/// The stores still run where they are reached, and nothing reads them.
#[test]
fn a_block_ending_in_an_unused_val_still_runs() {
    common::expect_box_ok_with_stdlib(
        "fun store(x: Boolean): Int {\n\
         \x20   if (x) {\n\
         \x20       val unused = 5\n\
         \x20   }\n\
         \x20   return 1\n\
         }\n\
         fun box(): String {\n\
         \x20   if (store(true) != 1) return \"true\"\n\
         \x20   return if (store(false) == 1) \"OK\" else \"false\"\n\
         }\n",
        "EmptyLocalBox",
    );
}
