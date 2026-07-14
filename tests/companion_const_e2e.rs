//! Companion-object `const val`: a compile-time-literal `const val` in a `companion object` becomes a
//! `public static final` + `ConstantValue` field on the OUTER class (kotlinc's layout); a `C.X` read is
//! `getstatic C.X`. Previously a companion with ANY property bailed the whole file. A companion with
//! both `const val`s and methods works (the const fields on C, methods on `C$Companion`). Round-tripped.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn companion_const_read() {
    const SRC: &str = "class C { companion object { const val X = \"OK\" } }\n\
fun box(): String = C.X\n";
    assert_eq!(run(SRC).expect("companion const compiles + runs"), "OK");
}

#[test]
fn companion_const_int_and_string() {
    const SRC: &str = "class C { companion object { const val X = \"OK\"; const val N = 42 } }\n\
fun box(): String = if (C.N == 42) C.X else \"no\"\n";
    assert_eq!(run(SRC).expect("companion consts compile + run"), "OK");
}

#[test]
fn companion_const_with_method() {
    const SRC: &str = "class C { companion object { const val X = \"O\"; fun make() = \"K\" } }\n\
fun box(): String = C.X + C.make()\n";
    assert_eq!(
        run(SRC).expect("companion const+method compiles + runs"),
        "OK"
    );
}

#[test]
fn user_type_shadows_builtin_companion_const() {
    const SRC: &str = "class Int\n\
fun box(): String = if (Int.MAX_VALUE == 0) \"bad\" else \"bad\"\n";
    assert!(
        run(SRC).is_none(),
        "user class Int must not fall through to kotlin.Int.MAX_VALUE"
    );
}

#[test]
fn companion_const_read_unqualified_from_member() {
    // A companion `const val` read UNQUALIFIED from a regular member (`fun f() = HEX`, not `C.HEX`).
    // The checker only resolved companion consts inside a companion member; the IR backend only read
    // them via the qualified `C.HEX` path — so the bare form failed to resolve and, once resolved,
    // found no instance field/getter and bailed the whole file. Both paths now inline the literal.
    const SRC: &str = "class C {\n\
    companion object { private const val HEX = 16; private const val LEN = 8 }\n\
    fun f(n: Int): String = n.toString(HEX).take(LEN)\n\
}\n\
fun box(): String = if (C().f(255) == \"ff\") \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("bare companion const read compiles + runs"),
        "OK"
    );
}

#[test]
fn companion_const_bare_and_qualified_agree() {
    // The bare and qualified reads of the same companion const must produce the same value.
    const SRC: &str = "class C {\n\
    companion object { const val N = 42 }\n\
    fun bare(): Int = N\n\
}\n\
fun box(): String = if (C().bare() == 42 && C.N == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("bare vs qualified companion const agree"),
        "OK"
    );
}
