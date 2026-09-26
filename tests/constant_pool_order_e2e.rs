//! The constant pool of a method kotlinc's bytecode passes rewrote, as kotlinc's writer builds it
//! from the optimized method: an instruction a pass removed leaves no entry behind, and one a pass
//! introduced finds the entries it names instead of keeping the method as emitted.
use super::common;

/// Compile `src` with kotlinc and krusty and require `class` to be the same bytes.
fn byte_identical(name: &str, src: &str, class: &str) {
    common::byte_diff_against_kotlinc(name, src, class)
        .expect("the reference kotlinc is required")
        .unwrap_or_else(|difference| panic!("{difference}"));
}

/// `x as String` of a local holding a string: the cast goes, and `java/lang/String` is named by
/// nothing else, so kotlinc's pool has no entry for it.
#[test]
fn a_removed_cast_leaves_no_entry() {
    byte_identical(
        "RemovedCast",
        "fun box(): String {\n\
         \x20   val x: Any = \"OK\"\n\
         \x20   return x as String\n\
         }\n",
        "RemovedCastKt",
    );
}

/// Casts of constants to their own boxed types: the boxing passes leave a method that names
/// constants the emitted one did not.
#[test]
fn a_rewrite_naming_new_constants_is_written() {
    byte_identical(
        "UnboxedCasts",
        "fun box(): String {\n\
         \x20   val b = true as? Boolean\n\
         \x20   val i = 1 as Int\n\
         \x20   val j = 1 as Int?\n\
         \x20   val s = \"s\" as String\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnboxedCastsKt",
    );
}

/// A generic property read at `Int`, stored as `Any` and tested with `is Int`.
#[test]
fn an_unboxed_generic_read_is_written() {
    byte_identical(
        "UnboxedGeneric",
        "class Cell<T>(var a: T)\n\
         fun read() = Cell<Int>(5).a\n\
         fun box(): String {\n\
         \x20   val x: Any = read()\n\
         \x20   return if (x is Int) \"OK\" else \"Fail\"\n\
         }\n",
        "UnboxedGenericKt",
    );
}
