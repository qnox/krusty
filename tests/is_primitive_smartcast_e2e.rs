//! Non-null primitive `is` smart-cast + boxed-FP `==`. `if (x is Double && y is Double) x == y`
//! narrows both to the primitive (the checker), and a USE unboxes (the lowerer's `Name` path coerces
//! a reference slot to the narrowed primitive) → an IEEE `dcmp` (`0.0 == -0.0` is true). Also covers a
//! `Char` operator-method result boxed into `Any` (`'A'.plus(1)` is `Char`, must box as `Character`).
//! Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn is_double_smartcast_ieee_eq() {
    // `0.0 == -0.0` is true under IEEE primitive `==`; both null/other-type paths handled.
    const SRC: &str = "fun eq(x: Any, y: Any) = x is Double && y is Double && x == y\n\
fun box(): String {\n\
    if (!eq(0.0, -0.0)) return \"f1\"\n\
    if (eq(0.0, 1.0)) return \"f2\"\n\
    if (eq(0.0, \"x\")) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("is Double smartcast eq"), "OK");
}

#[test]
fn is_int_smartcast_use() {
    // `a is Int` narrows `a` to `Int`; `a + 1` unboxes and adds.
    const SRC: &str = "fun f(a: Any): Int { if (a is Int) return a + 1; return -1 }\n\
fun box(): String = if (f(41) == 42 && f(\"x\") == -1) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("is Int smartcast use"), "OK");
}

#[test]
fn char_op_method_boxes_as_character() {
    // `'A'.plus(1)` is a `Char` (`Char.plus(Int): Char`) — stored in `Any` it must box as `Character`,
    // so `is Char` holds and the value is `'B'`. `'B'.minus('A')` is an `Int`.
    const SRC: &str = "fun box(): String {\n\
    val a: Any = 'A'.plus(1)\n\
    if (a !is Char || a != 'B') return \"f1\"\n\
    val b: Any = 'B'.minus('A')\n\
    if (b !is Int || b != 1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("char op-method boxing"), "OK");
}
