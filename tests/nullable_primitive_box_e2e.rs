//! Boxed nullable-primitive codegen (`Int?`, `Short?`, …). These pin the representation that broke when
//! a nullable primitive stopped being faked as its JVM wrapper `Obj` and became `Ty::Nullable`: `ty_to_ir`
//! had no `Nullable` arm, so a nullable primitive lowered to `Ty::Error` and the JVM backend emitted
//! unverifiable bytecode (`VerifyError: Bad type on operand stack`) — ~57 codegen/box corpus cases. The
//! hand-written e2e missed it; this file covers the boxing/comparison/data-class/elvis/return patterns the
//! corpus exercises, round-tripped on the JVM.

mod common;

#[test]
fn boxed_nullable_int_equality() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Int? = 5\n\
    val b: Int? = 5\n\
    if (a != b) return \"fail eq\"\n\
    val c: Int? = null\n\
    if (a == c) return \"fail null-eq\"\n\
    if (c != null) return \"fail null\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn not_null_assert_on_nullable_int_arith() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Int? = 7\n\
    val r = a!! + 1\n\
    return if (r == 8) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn elvis_on_nullable_int() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Int? = null\n\
    if ((a ?: 0) != 0) return \"fail null\"\n\
    val b: Int? = 9\n\
    if ((b ?: 0) != 9) return \"fail nonnull\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn nullable_int_returned_from_function() {
    const SRC: &str = "// WITH_STDLIB\n\
fun f(b: Boolean): Int? = if (b) 42 else null\n\
fun box(): String {\n\
    if (f(true) != 42) return \"fail true\"\n\
    if (f(false) != null) return \"fail false\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn data_class_with_nullable_int_field() {
    const SRC: &str = "// WITH_STDLIB\n\
data class P(val x: Int?, val y: String)\n\
fun box(): String {\n\
    val p = P(null, \"z\")\n\
    if (p.x != null) return \"fail x-null\"\n\
    val q = P(3, \"z\")\n\
    if (q.x != 3) return \"fail q-x\"\n\
    if (p == q) return \"fail eq\"\n\
    p.hashCode()\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn boxed_nullable_long() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Long? = 1L\n\
    if (a == null) return \"fail null\"\n\
    if (a != 1L) return \"fail val\"\n\
    val c: Long? = null\n\
    if (c != null) return \"fail c\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}
