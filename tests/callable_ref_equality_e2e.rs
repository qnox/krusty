//! Callable references are emitted as `kotlin/jvm/internal/FunctionReferenceImpl` subclasses (not bare
//! `LambdaMetafactory` closures), so they carry real Kotlin reference EQUALITY: two references to the
//! same top-level function are equal; a bound member reference equals another with the SAME receiver but
//! differs from one with a different receiver and from the unbound reference. Round-tripped under
//! `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn function_reference_equality() {
    const SRC: &str = "fun top1(p: String) {}\n\
fun top2(p: String) {}\n\
class Foo { fun mem(p: String) {} }\n\
fun ckEq(x: Any, y: Any) { if (x != y || x.hashCode() != y.hashCode()) throw AssertionError(\"$x != $y\") }\n\
fun ckNe(x: Any, y: Any) { if (x == y) throw AssertionError(\"$x == $y\") }\n\
fun box(): String {\n\
    ckEq(::top1, ::top1)\n\
    ckNe(::top1, ::top2)\n\
    val foo = Foo()\n\
    val bar = Foo()\n\
    ckEq(foo::mem, foo::mem)\n\
    ckNe(foo::mem, bar::mem)\n\
    ckNe(foo::mem, Foo::mem)\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("function reference equality should hold");
    assert_eq!(out, "OK");
}

#[test]
fn function_reference_still_invokes() {
    // The FunctionReferenceImpl subclass must still work as a Function in a higher-order call, with a
    // value-returning target, an unbound member, and a bound member.
    const SRC: &str = "fun twice(x: Int) = x * 2\n\
class Acc(val base: Int) { fun add(x: Int) = base + x }\n\
fun ap(f: (Int) -> Int, v: Int) = f(v)\n\
fun ap2(f: (Acc, Int) -> Int, a: Acc, v: Int) = f(a, v)\n\
fun box(): String {\n\
    if (ap(::twice, 5) != 10) return \"fail top\"\n\
    val acc = Acc(100)\n\
    if (ap(acc::add, 7) != 107) return \"fail bound\"\n\
    if (ap2(Acc::add, Acc(1), 2) != 3) return \"fail unbound\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("function references should still invoke");
    assert_eq!(out, "OK");
}

#[test]
fn local_function_reference_keeps_multiple_captures_and_declaration_equality() {
    const SRC: &str = r#"
class Host(private val first: String) {
    fun run(): String {
        val second = "K"
        fun selected(): String = first + second
        if (::selected != ::selected) return "FAIL: equality"
        return (::selected)()
    }
}

fun box(): String = Host("O").run()
"#;
    let out = run(SRC).expect("multi-capture local function reference should compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn local_function_reference_equality_ignores_ordinary_capture_values() {
    const SRC: &str = r#"
fun make(value: String): () -> String {
    fun selected(): String = value
    return ::selected
}

fun box(): String {
    val first = make("O")
    val second = make("X")
    if (first != second) return "FAIL: equality"
    return if (first() == "O" && second() == "X") "OK" else "FAIL: invocation"
}
"#;
    let out = run(SRC).expect("ordinary local captures should not become bound receivers");
    assert_eq!(out, "OK");
}

#[test]
fn local_extension_reference_separates_captures_from_its_bound_receiver() {
    const SRC: &str = r#"
fun make(prefix: String, receiver: String): () -> String {
    fun String.selected(): String = prefix + this
    return receiver::selected
}

fun box(): String {
    val first = make("A", "O")
    val sameReceiver = make("B", "O")
    val otherReceiver = make("A", "X")
    if (first != sameReceiver) return "FAIL: ordinary capture affected equality"
    if (first == otherReceiver) return "FAIL: bound receiver ignored"
    return if (first() == "AO" && sameReceiver() == "BO" && otherReceiver() == "AX") {
        "OK"
    } else {
        "FAIL: invocation"
    }
}
"#;
    let out = run(SRC).expect("local extension reference should preserve both capture kinds");
    assert_eq!(out, "OK");
}

#[test]
fn suspend_top_level_reference_keeps_identity_and_continuation_shape() {
    const SRC: &str = "import kotlin.coroutines.*\n\
suspend fun twice(x: Int): Int = x * 2\n\
class SuspendAcc(private val base: Int) { suspend fun plus(x: Int): Int = base + x }\n\
fun ckEq(x: Any, y: Any) { if (x != y || x.hashCode() != y.hashCode()) throw AssertionError(\"$x != $y\") }\n\
suspend fun probe(): String {\n\
    ckEq(::twice, ::twice)\n\
    val acc = SuspendAcc(1)\n\
    ckEq(acc::plus, acc::plus)\n\
    val reference = ::twice\n\
    val bound = acc::plus\n\
    val unbound = SuspendAcc::plus\n\
    val a = reference(10)\n\
    val b = bound(10)\n\
    val c = unbound(acc, 10)\n\
    val result = a + b + c\n\
    return if (result == 42) \"OK\" else \"fail: $result\"\n\
}\n";
    common::expect_suspend_result(
        "SuspendCallableReferenceIdentity",
        SRC,
        "probe(continuation)",
        "OK",
    );
}
