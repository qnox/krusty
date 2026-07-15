//! Cast of a PRIMITIVE operand to a reference type (`42 as Any`, `'a' as Char?`, `b as Byte?`). This
//! is a boxing operation — the primitive is boxed to its wrapper (`Integer`/`Character`/`Byte`), which
//! is-a the (always assignable, per the checker) reference target. Round-tripped on the JVM.

use super::common;

#[test]
fn primitive_to_different_primitive_cast_throws_cce() {
    // `x as Byte` where `x` is a different primitive (`Int`) is a CHECKED cast: box `x`, `checkcast`
    // the target wrapper (CCE — `Integer` is not `Byte`), unbox. Same-primitive is identity.
    const SRC: &str = "// WITH_STDLIB\n\
fun <T> check(param: T, f: (T) -> Unit): String {\n\
    try { f(param) } catch (e: ClassCastException) { return \"CCE\" }\n\
    return \"ok\"\n\
}\n\
fun box(): String {\n\
    if (check(1, { it as Int }) != \"ok\") return \"fail same\"\n\
    if (check(1, { it as Byte }) != \"CCE\") return \"fail byte\"\n\
    if (check(1, { it as Long }) != \"CCE\") return \"fail long\"\n\
    if (check(1.0, { it as Int }) != \"CCE\") return \"fail dbl2int\"\n\
    if (check(1.0, { it as Double }) != \"ok\") return \"fail dbl\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn primitive_cast_in_inline_generic_hof() {
    // `x as Int`/`x as String` inside an inline generic HOF: the type-parameter operand is a
    // specialized primitive int in a slot the checker views as erased; the `is` check boxes it, the
    // cast is identity, and the block result boxes to the erased return before the inline return.
    const SRC: &str = "// WITH_STDLIB\n\
inline fun <R, T> foo(x: R, y: R, block: (R) -> T): T {\n\
    val a = x is Number\n\
    if (a) return block(x) else return block(y)\n\
}\n\
fun box(): String {\n\
    if (foo(1, 2) { x -> x as Int } != 1) return \"fail int\"\n\
    if (foo(\"abc\", \"def\") { x -> x as String } != \"def\") return \"fail str\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn inline_hof_prim_operand_is_and_as_object() {
    // A specialized-primitive type-parameter operand feeding `is Number`, `is Object`, `as Object`
    // (box to reference), and `as Int` (identity) inside an inline generic HOF, with the erased result
    // boxed at the inline return — the reconstructed `checkcastAndInstanceOf` box case.
    const SRC: &str = "// WITH_STDLIB\n\
inline fun <R, T> foo(x : R, y : R, block : (R) -> T) : T {\n\
    val a = x is Number\n\
    val b = x is Object\n\
    val b1 = x as Object\n\
    if (a && b) { return block(x) } else { return block(y) }\n\
}\n\
fun box() : String {\n\
    if (foo(1, 2) { x -> x as Int } != 1) return \"f1\"\n\
    if (foo(\"abc\", \"def\") { x -> x as String } != \"def\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn is_check_on_primitive_operand_boxes() {
    // `x is Number` where an inline function's generic type parameter is specialized to a primitive
    // (`x: T`, `T = Int`): the operand is a raw `int` in the slot, so it must be BOXED before
    // `instanceof` (a raw scalar there VerifyErrors).
    const SRC: &str = "// WITH_STDLIB\n\
inline fun <T> isNum(x: T): Boolean = x is Number\n\
fun box(): String {\n\
    if (!isNum(1)) return \"fail int\"\n\
    if (!isNum(2.0)) return \"fail double\"\n\
    if (isNum(\"s\")) return \"fail str\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn primitive_to_reference_cast_boxes() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val a: Any = 42 as Any\n\
    if (a != 42) return \"fail any\"\n\
    val c = 'x' as Char?\n\
    if (c != 'x') return \"fail char\"\n\
    val b: Byte = 10\n\
    val bn = b as Byte?\n\
    if (bn!!.toInt() != 10) return \"fail byte\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn impossible_primitive_cast_is_rejected_not_miscompiled() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    // `1 as String` can never succeed (kotlinc rejects it). krusty must NOT box an `Integer` into a
    // `String` slot (a load-time VerifyError) — it rejects the file (compile returns `None`) instead.
    let jh = common::java_home().unwrap();
    let sl = common::stdlib_jar().unwrap();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let src = "// WITH_STDLIB\nfun box(): String { val s = 1 as String; return s }\n";
    assert!(
        common::compile_in_process(src, "P", &[sl], Some(&jdk)).is_none(),
        "impossible primitive→String cast must be rejected, not compiled to broken bytecode"
    );
}
