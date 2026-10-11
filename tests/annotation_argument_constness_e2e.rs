//! A non-constant annotation argument is reported on the argument, with kotlinc's diagnostic for
//! its kind: a `val` that could have been `const`, a class literal of a value, or any other
//! non-constant expression. Every ledger is compared with kotlinc's in full.

use super::common;

/// A class literal whose receiver failed to resolve is not a constant; one whose receiver is a
/// value is not a class literal.
#[test]
fn non_constant_class_literal_arguments_are_reported_on_the_argument() {
    let src = "annotation class C(val k: kotlin.reflect.KClass<*>)
@C(Unknown::class) fun unresolved() {}
fun value(o: Any) { @C(o::class) val y = 1 }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A module `val` with a default getter and a literal initializer could have been `const`, however
/// it is reached: by name, through `this`, an instance, an object or a companion.
#[test]
fn a_val_with_a_literal_initializer_could_have_been_const() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
annotation class L(val l: Long)
annotation class B(val b: Boolean)
val text = \"a\"
val negative = -1
val parenthesized = (2)
val widened: Long = 1
val flag = true
val raw = \"\"\"raw\"\"\"
class C {
    val member = \"a\"
    private val hidden = \"x\"
    fun f() { @A(member) val y = 1; @A(this.member) val z = 1 }
    @A(hidden) fun g() {}
    companion object { val shared = \"a\" }
}
object O { val single = \"a\" }
fun instance(c: C) { @A(c.member) val y = 1 }
@A(text) fun a1() {}
@I(negative) fun a2() {}
@I(parenthesized) fun a3() {}
@L(widened) fun a4() {}
@B(flag) fun a5() {}
@A(raw) fun a6() {}
@A(O.single) fun a7() {}
@A(C.shared) fun a8() {}
@A((text)) fun a9() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Any other read is not a compile-time constant: a `var`, a custom or delegated getter, a
/// non-literal initializer, a local, a parameter, a constructor property, a dependency's property,
/// a branch, and a call of a module function.
#[test]
fn other_non_constant_reads_are_not_constants() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
fun s() = \"a\"
const val k = \"k\"
var mutable = \"a\"
val getter get() = \"a\"
val delegated: String by lazy { \"a\" }
val computed = s()
val concatenated = \"a\" + \"b\"
val aliased = k
lateinit var late: String
val String.extension get() = \"a\"
class C(val parameter: String) { companion object { var counter = \"a\" } }
@A(mutable) fun a1() {}
@A(getter) fun a2() {}
@A(delegated) fun a3() {}
@A(computed) fun a4() {}
@A(concatenated) fun a5() {}
@A(aliased) fun a6() {}
@A(late) fun a7() {}
@A(\"a\".extension) fun a8() {}
@A(C.counter) fun a9() {}
@I(KotlinVersion.CURRENT.major) fun a10() {}
@A(java.io.File.separator) fun a11() {}
@A(if (true) \"a\" else \"b\") fun a12() {}
@A(s()) fun a13() {}
fun locals(q: String, c: C) {
    val l = \"a\"
    @A(q) val x = 1
    @A(l) val y = 1
    @A(c.parameter) val z = 1
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// An operator is as constant as its first non-constant operand, and the argument carries that
/// operand's diagnostic.
#[test]
fn an_operator_reports_its_first_non_constant_operand() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
fun s() = \"a\"
val text = \"a\"
var mutable = \"a\"
@A(text + \"b\") fun a1() {}
@A(\"x\" + text) fun a2() {}
@A(text + mutable) fun a3() {}
@A(mutable + text) fun a4() {}
@A(\"a\" + s()) fun a5() {}
@I(-KotlinVersion.CURRENT.major) fun a6() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}
