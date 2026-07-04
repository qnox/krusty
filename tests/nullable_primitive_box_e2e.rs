//! Boxed nullable-primitive codegen (`Int?`, `Short?`, …). These pin the representation that broke when
//! a nullable primitive stopped being faked as its JVM wrapper `Obj` and became `Ty::Nullable`: `ty_to_ir`
//! had no `Nullable` arm, so a nullable primitive lowered to `Ty::Error` and the JVM backend emitted
//! unverifiable bytecode (`VerifyError: Bad type on operand stack`) — ~57 codegen/box corpus cases. The
//! hand-written e2e missed it; this file covers the boxing/comparison/data-class/elvis/return patterns the
//! corpus exercises, round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

/// Skip (not fail) when the JVM + stdlib jar this e2e needs is absent.
fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn boxed_nullable_int_equality() {
    if !toolchain_ready() {
        return;
    }
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
    assert_eq!(
        run(SRC).expect("boxed Int? equality should compile + run"),
        "OK"
    );
}

#[test]
fn not_null_assert_on_nullable_int_arith() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Int? = 7\n\
    val r = a!! + 1\n\
    return if (r == 8) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("`Int?!! + 1` should compile + run"), "OK");
}

#[test]
fn elvis_on_nullable_int() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Int? = null\n\
    if ((a ?: 0) != 0) return \"fail null\"\n\
    val b: Int? = 9\n\
    if ((b ?: 0) != 9) return \"fail nonnull\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("`Int? ?: 0` should compile + run"), "OK");
}

#[test]
fn nullable_int_returned_from_function() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
fun f(b: Boolean): Int? = if (b) 42 else null\n\
fun box(): String {\n\
    if (f(true) != 42) return \"fail true\"\n\
    if (f(false) != null) return \"fail false\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("a `: Int?` return should compile + run"),
        "OK"
    );
}

#[test]
fn data_class_with_nullable_int_field() {
    if !toolchain_ready() {
        return;
    }
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
    assert_eq!(
        run(SRC).expect("data class with an Int? field should compile + run"),
        "OK"
    );
}

#[test]
fn boxed_nullable_long() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Long? = 1L\n\
    if (a == null) return \"fail null\"\n\
    if (a != 1L) return \"fail val\"\n\
    val c: Long? = null\n\
    if (c != null) return \"fail c\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("a boxed `Long?` (the two-slot wrapper path) should compile + run"),
        "OK"
    );
}
