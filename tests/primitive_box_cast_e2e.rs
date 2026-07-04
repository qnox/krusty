//! Cast of a PRIMITIVE operand to a reference type (`42 as Any`, `'a' as Char?`, `b as Byte?`). This
//! is a boxing operation — the primitive is boxed to its wrapper (`Integer`/`Character`/`Byte`), which
//! is-a the (always assignable, per the checker) reference target. Round-tripped on the JVM.

use super::common;

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
