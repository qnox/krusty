//! An in-class `lateinit` FIELD read in operand position. The read emits `dup; ifnonnull L; ldc name;
//! invokestatic throwUninitializedPropertyAccessException; L:` — a branch whose join records a stackmap
//! frame — so any operand already on the stack when it runs must be spilled to a temp first, or the
//! frame types only the field value and the JVM verifier rejects the class at link time.
//!
//! These must RUN, not merely compile: the emitted class file is well-formed, and the mismatch only
//! surfaces as a `VerifyError` when the JVM links the method.

use super::common;

#[test]
fn lateinit_field_read_in_vararg_operand() {
    // The `Object[]` for `listOf`'s vararg (plus its index) is held on the stack across the read.
    const SRC: &str = "class C {\n\
    lateinit var s: String\n\
    fun f(): List<String> = listOf(s, s)\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"OK\"\n\
    return if (c.f() == listOf(\"OK\", \"OK\")) \"OK\" else \"FAIL: ${c.f()}\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn lateinit_field_read_as_later_call_argument() {
    // No array anywhere: the earlier argument `"x"` alone is live across the read.
    const SRC: &str = "fun k(a: String, b: String): String = a + b\n\
class C {\n\
    lateinit var s: String\n\
    fun f(): String = k(\"O\", s)\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"K\"\n\
    return c.f()\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn lateinit_field_read_in_array_subscript() {
    // The array reference is live across the read that computes the index.
    const SRC: &str = "class C {\n\
    lateinit var s: String\n\
    fun f(a: Array<String>): String = a[s.length - 2]\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"abc\"\n\
    return c.f(arrayOf(\"FAIL\", \"OK\", \"FAIL\"))\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn private_lateinit_field_read_in_operand() {
    // A private `lateinit` is read straight off the field from inside the class — no accessor to hide
    // the null check behind.
    const SRC: &str = "class C {\n\
    private lateinit var s: String\n\
    fun init() { s = \"OK\" }\n\
    fun f(): List<String> = listOf(s, s)\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.init()\n\
    return c.f()[0]\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn lateinit_field_read_in_constructor_operand() {
    // `New` builds its argument list on the stack the same way a call does.
    const SRC: &str = "class P(val a: String, val b: String)\n\
class C {\n\
    lateinit var s: String\n\
    fun f(): P = P(\"O\", s)\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"K\"\n\
    val p = c.f()\n\
    return p.a + p.b\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn lateinit_field_read_in_string_concat() {
    // The `StringBuilder` is live on the stack across the read.
    const SRC: &str = "class C {\n\
    lateinit var s: String\n\
    fun f(): String = \"O\" + s + \"!\"\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"K\"\n\
    return if (c.f() == \"OK!\") \"OK\" else \"FAIL: ${c.f()}\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn lateinit_field_read_in_field_write_value() {
    // `SetField` holds the receiver across the value expression.
    const SRC: &str = "class C {\n\
    lateinit var s: String\n\
    var t: String = \"\"\n\
    fun f(): String { t = s; return t }\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.s = \"OK\"\n\
    return c.f()\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn uninitialized_lateinit_field_read_in_operand_still_throws() {
    // Spilling must not swallow the guard: the read still throws while the field is null.
    const SRC: &str = "class C {\n\
    lateinit var s: String\n\
    fun f(): List<String> = listOf(s, s)\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    try {\n\
        val r = c.f()\n\
        return \"FAIL: no throw, got $r\"\n\
    } catch (e: RuntimeException) {\n\
        return \"OK\"\n\
    }\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}
