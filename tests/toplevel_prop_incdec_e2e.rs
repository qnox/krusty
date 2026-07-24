//! `prop++` / `--prop` on a TOP-LEVEL `var` — statement and expression position. The property is a
//! static field of the file class (or a `getX`/`setX` accessor pair when it has a custom setter, or
//! the other file's facade accessors cross-file), so the local-slot IncDec lowering does not apply:
//! the read/update/write must route through `getstatic`/`putstatic` or the accessors.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn stmt_incdec_on_toplevel_var() {
    const SRC: &str = "var count = 0\n\
var down = 10L\n\
fun bump() { count++ }\n\
fun drop2() { down--; down-- }\n\
fun box(): String {\n\
    bump(); bump(); bump()\n\
    drop2()\n\
    if (count != 3) return \"FAIL count=$count\"\n\
    if (down != 8L) return \"FAIL down=$down\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn stmt_incdec_on_toplevel_var_from_class_method() {
    // Bare `count++` inside a class METHOD where the class has no `count` member — binds to the
    // top-level property (the member path must fall through, not bail).
    const SRC: &str = "var count = 0\n\
class A {\n\
    fun bump() { count++ }\n\
}\n\
fun box(): String {\n\
    val a = A()\n\
    a.bump(); a.bump()\n\
    return if (count == 2) \"OK\" else \"FAIL count=$count\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn expr_incdec_on_toplevel_var() {
    // Postfix yields the OLD value, prefix the NEW one; the write still lands in the static.
    const SRC: &str = "var n = 5\n\
fun box(): String {\n\
    val old = n++\n\
    if (old != 5 || n != 6) return \"FAIL post old=$old n=$n\"\n\
    val new2 = ++n\n\
    if (new2 != 7 || n != 7) return \"FAIL pre new=$new2 n=$n\"\n\
    val od = n--\n\
    if (od != 7 || n != 6) return \"FAIL postdec od=$od n=$n\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn stmt_incdec_on_toplevel_short() {
    // A narrowing scalar (Short) wraps in its own width on update.
    const SRC: &str = "var s: Short = 32766\n\
fun box(): String {\n\
    s++\n\
    s++\n\
    return if (s == (-32768).toShort()) \"OK\" else \"FAIL s=$s\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn stmt_incdec_on_toplevel_var_with_custom_setter() {
    // A custom setter must RUN on `count++` (kotlinc routes the write through `setX`).
    const SRC: &str = "var log = \"\"\n\
var count: Int = 0\n\
    set(value) { log += \"set($value);\"; field = value }\n\
fun box(): String {\n\
    count++\n\
    count++\n\
    if (count != 2) return \"FAIL count=$count\"\n\
    return if (log == \"set(1);set(2);\") \"OK\" else \"FAIL log=$log\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn expr_incdec_in_argument_and_template_position() {
    // Postfix as a call argument and inside a string template — the temp-spill `Block` must verify
    // in an operand position too.
    const SRC: &str = "var n = 1\n\
fun take(x: Int, y: Int): Int = x * 10 + y\n\
fun box(): String {\n\
    val r = take(n++, n)\n\
    if (r != 12) return \"FAIL r=$r\"\n\
    val s = \"v=${n--}\"\n\
    if (s != \"v=2\" || n != 1) return \"FAIL s=$s n=$n\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}

#[test]
fn toplevel_delegated_var_incdec_skips() {
    // A top-level delegated `var` has NO writable static/setter storage modeled — `x++` must bail
    // to a skip (it used to be unreachable; a panic here is a regression). Flips to a real test
    // when delegated-property writes land.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class D { var v = 5\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = v\n\
    operator fun setValue(t: Any?, p: KProperty<*>, value: Int) { v = value }\n\
}\n\
var x: Int by D()\n\
fun box(): String { x++; return if (x == 6) \"OK\" else \"FAIL\" }\n";
    assert!(
        run(SRC).is_none(),
        "top-level delegated-var ++ unexpectedly compiled — promote this to a positive test"
    );
}

#[test]
fn toplevel_unsigned_var_incdec_skips() {
    // `UInt` stores a value-class-wrapped value; a raw primitive add on the static would
    // miscompile — must bail to a skip.
    const SRC: &str = "var u: UInt = 5u\n\
fun box(): String { u++; return if (u == 6u) \"OK\" else \"FAIL\" }\n";
    assert!(
        run(SRC).is_none(),
        "top-level UInt ++ unexpectedly compiled — promote this to a positive test"
    );
}

#[test]
fn member_shadowing_toplevel_var_binds_member() {
    // A class MEMBER `count` shadows the same-named top-level `var` inside the class's methods
    // (kotlinc scoping) — the member must be updated, the top-level left alone.
    const SRC: &str = "var count = 100\n\
class A {\n\
    var count = 0\n\
    fun bump() { count++ }\n\
}\n\
fun box(): String {\n\
    val a = A()\n\
    a.bump()\n\
    if (a.count != 1) return \"FAIL member=${a.count}\"\n\
    return if (count == 100) \"OK\" else \"FAIL toplevel=$count\"\n\
}\n";
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn apply_receiver_does_not_unhide_outer_member() {
    // Inside `Box().apply { … }` in a method of C, `count++` must bind C's MEMBER (the outer
    // implicit receiver) — never the same-named top-level `var`, even though the innermost
    // receiver (Box) lacks the name. A conservative skip is acceptable; a top-level write is not.
    const SRC: &str = "var count = 100\n\
class Box\n\
class C {\n\
    var count = 0\n\
    fun f() { Box().apply { count++ } }\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    c.f()\n\
    if (count != 100) return \"FAIL toplevel=$count\"\n\
    return if (c.count == 1) \"OK\" else \"FAIL member=${c.count}\"\n\
}\n";
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn companion_member_shadows_toplevel_var() {
    // A companion `var count` shadows the top-level one inside the class's methods (class-body
    // scope wins over file scope). A conservative skip is acceptable; a top-level write is not.
    const SRC: &str = "var count = 100\n\
class C {\n\
    companion object { var count = 0 }\n\
    fun f() { count++ }\n\
}\n\
fun box(): String {\n\
    C().f()\n\
    if (count != 100) return \"FAIL toplevel=$count\"\n\
    return if (C.count == 1) \"OK\" else \"FAIL companion=${C.count}\"\n\
}\n";
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn stmt_incdec_on_toplevel_var_cross_file() {
    // The var lives in ANOTHER file of the same module — the write goes through the facade's
    // `getX`/`setX` (its backing field is private).
    const SRC: &str = "// FILE: lib.kt\n\
var shared = 0\n\
// FILE: main.kt\n\
fun box(): String {\n\
    shared++\n\
    shared++\n\
    shared--\n\
    return if (shared == 1) \"OK\" else \"FAIL shared=$shared\"\n\
}\n";
    assert_eq!(run(SRC).expect("compiles + runs"), "OK");
}
