//! A non-null type parameter bounded by a JVM primitive is that primitive on the real method
//! and the JDK wrapper on the `$default` stub. `T : Char` therefore calls
//! `test$nested$default(Test, Character, int, Object)` with `Character.valueOf`, and the stub
//! unboxes with `charValue` before the real `char` method. The same boundary holds for a member.

use super::common;

const LOCAL: &str = "\
class Test<T : Char>(val k: T) {\n\
    fun test(): String {\n\
        fun nested(x: T = k): String = \"O$x\"\n\
        return nested()\n\
    }\n\
}\n\
fun box(): String = Test('K').test()\n\
";

const MEMBER: &str = "\
class Test<T : Char>(val k: T) {\n\
    fun nested(x: T = k): String = \"O$x\"\n\
    fun test(): String = nested()\n\
}\n\
fun box(): String = Test('K').test()\n\
";

const OTHER_PRIMITIVES: &str = "\
class BooleanDefault<T : Boolean>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class ByteDefault<T : Byte>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class ShortDefault<T : Short>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class IntDefault<T : Int>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class LongDefault<T : Long>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class FloatDefault<T : Float>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
class DoubleDefault<T : Double>(private val fallback: T) {\n\
    fun value(x: T = fallback): T = x\n\
}\n\
fun box(): String {\n\
    if (!BooleanDefault(true).value()) return \"Boolean\"\n\
    if (ByteDefault(1.toByte()).value() != 1.toByte()) return \"Byte\"\n\
    if (ShortDefault(2.toShort()).value() != 2.toShort()) return \"Short\"\n\
    if (IntDefault(3).value() != 3) return \"Int\"\n\
    if (LongDefault(4L).value() != 4L) return \"Long\"\n\
    if (FloatDefault(5.0f).value() != 5.0f) return \"Float\"\n\
    if (DoubleDefault(6.0).value() != 6.0) return \"Double\"\n\
    return \"OK\"\n\
}\n\
";

fn assert_same_instructions(stem: &str, src: &str, class: &str, member: &str) {
    let built =
        common::compare_with_kotlinc_plugin(stem, src, class, &[common::stdlib_jar()], "25", &[])
            .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn a_local_primitive_bound_default_boxes_like_kotlinc() {
    assert_same_instructions(
        "PrimitiveBoundLocalDefault",
        LOCAL,
        "Test",
        "java.lang.String test();",
    );
    assert_same_instructions(
        "PrimitiveBoundLocalDefault",
        LOCAL,
        "Test",
        "java.lang.String test$nested$default(Test, java.lang.Character, int, java.lang.Object);",
    );
}

#[test]
fn a_member_primitive_bound_default_boxes_like_kotlinc() {
    assert_same_instructions(
        "PrimitiveBoundMemberDefault",
        MEMBER,
        "Test",
        "java.lang.String test();",
    );
    assert_same_instructions(
        "PrimitiveBoundMemberDefault",
        MEMBER,
        "Test",
        "java.lang.String nested$default(Test, java.lang.Character, int, java.lang.Object);",
    );
}

#[test]
fn a_primitive_bound_default_runs() {
    common::expect_box_ok_with_stdlib(LOCAL, "PrimitiveBoundLocalDefault");
    common::expect_box_ok_with_stdlib(MEMBER, "PrimitiveBoundMemberDefault");
}

#[test]
fn every_other_jvm_primitive_bound_default_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(OTHER_PRIMITIVES, "PrimitiveBoundOtherDefaults");
}

#[test]
fn a_supplied_primitive_bound_argument_uses_the_sibling_default_stub_wrapper() {
    let sources = [
        (
            "Defaults.kt",
            "package primitivebounds\n\
             class Test<T : Char>(private val fallback: T) {\n\
                 fun join(value: T = fallback, suffix: String = \"K\") = \"$value$suffix\"\n\
             }\n",
        ),
        (
            "Main.kt",
            "package primitivebounds\n\
             fun box(): String = Test('X').join('O')\n",
        ),
    ];
    common::expect_box_ok_files_with_stdlib(
        &sources,
        "a supplied primitive-bound argument in a sibling default call",
    );
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "primitivebounds.MainKt"),
        "OK",
        "kotlinc reference for a sibling primitive-bound default call",
    );
}
