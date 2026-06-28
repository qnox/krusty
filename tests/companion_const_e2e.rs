//! Companion-object `const val`: a compile-time-literal `const val` in a `companion object` becomes a
//! `public static final` + `ConstantValue` field on the OUTER class (kotlinc's layout); a `C.X` read is
//! `getstatic C.X`. Previously a companion with ANY property bailed the whole file. A companion with
//! both `const val`s and methods works (the const fields on C, methods on `C$Companion`). Round-tripped.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
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
