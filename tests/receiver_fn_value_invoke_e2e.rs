//! Invoking a RECEIVER-function-typed value in lexical scope with member syntax: `b.f()` (and
//! `b?.f()`) where `f: Bar.() -> R` is a local/parameter and `Bar` has no member `f`. The receiver
//! becomes the function value's folded-first argument (`Function1.invoke(b)`). Mirrors corpus
//! `classes/kt1918.kt` (`(x as? Bar)?.bar()`).

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn plain_receiver_fn_param_invoke() {
    const SRC: &str = "class Bar { val v = 41 }\n\
fun call(b: Bar, f: Bar.() -> Int): Int = b.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { v + 1 }\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("plain receiver fn invoke"), "OK");
}

#[test]
fn safe_call_receiver_fn_param_invoke() {
    const SRC: &str = "class Bar { val v = 41 }\n\
fun call(b: Bar?, f: Bar.() -> Int): Int? = b?.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { v }\n\
    if (r != 41) return \"FAIL 1: $r\"\n\
    val n = call(null) { v }\n\
    if (n != null) return \"FAIL 2: $n\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("safe-call receiver fn invoke"), "OK");
}

#[test]
fn safe_cast_then_receiver_fn_invoke() {
    // The kt1918 shape: `(x as? Bar)?.bar()` where `bar` is a receiver-lambda parameter.
    const SRC: &str = "class Bar\n\
interface Foo { fun xyzzy(x: Any?): String }\n\
fun buildFoo(bar: Bar.() -> Unit): Foo {\n\
    return object : Foo {\n\
        override fun xyzzy(x: Any?): String {\n\
            (x as? Bar)?.bar()\n\
            return \"OK\"\n\
        }\n\
    }\n\
}\n\
fun box(): String {\n\
    val foo = buildFoo({})\n\
    return foo.xyzzy(Bar())\n\
}\n";
    assert_eq!(run(SRC).expect("safe-cast receiver fn invoke"), "OK");
}

#[test]
fn receiver_fn_with_value_args() {
    const SRC: &str = "class Acc { var total = 0 }\n\
fun apply2(a: Acc, op: Acc.(Int) -> Unit): Int {\n\
    a.op(40)\n\
    a.op(2)\n\
    return a.total\n\
}\n\
fun box(): String {\n\
    val r = apply2(Acc()) { n -> total += n }\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("receiver fn with args"), "OK");
}

#[test]
fn ctor_receiver_lambda_binds_implicit_this() {
    // KT-606: a receiver lambda passed to a CONSTRUCTOR parameter (`config: Pipeline.() -> Unit`)
    // binds the receiver as implicit `this` — a bare member call inside dispatches on the receiver,
    // not a same-named stdlib top-level (`print`).
    const SRC: &str = "var result = \"FAIL\"\n\
interface Pipeline { fun print(any: Any) }\n\
class Impl : Pipeline { override fun print(any: Any) { result = any as String } }\n\
class Factory(val config: Pipeline.() -> Unit) {\n\
    fun run(): Pipeline { val p: Pipeline = Impl(); p.config(); return p }\n\
}\n\
fun box(): String {\n\
    Factory({ print(\"OK\") }).run()\n\
    return result\n\
}\n";
    assert_eq!(run(SRC).expect("ctor receiver lambda"), "OK");
}

#[test]
fn suspend_receiver_fn_param_invoke() {
    // The suspend form: `f: suspend Bar.() -> Int` invoked with member syntax inside a suspend
    // function — the invoke is a suspension point (Function2.invoke with the continuation), so the
    // checker must mark it and the coroutine pass must thread the continuation.
    const SRC: &str = "import kotlin.coroutines.*\n\
class Bar { val v = 41 }\n\
suspend fun call(b: Bar, f: suspend Bar.() -> Int): Int = b.f()\n\
fun box(): String {\n\
    var res = 0\n\
    val block: suspend () -> Int = { call(Bar()) { v + 1 } }\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return if (res == 42) \"OK\" else \"FAIL: $res\"\n\
}\n";
    assert_eq!(run(SRC).expect("suspend receiver fn invoke"), "OK");
}

#[test]
fn suspend_receiver_fn_invoke_parks_and_resumes() {
    // The receiver-fn value REALLY suspends: park its continuation, ensure the enclosing state
    // machine re-enters after resume instead of falling through on COROUTINE_SUSPENDED.
    const SRC: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Unit>? = null\n\
var order = \"\"\n\
suspend fun pause(): Unit = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    order += \"parked;\"\n\
    COROUTINE_SUSPENDED\n\
}\n\
class Bar { val v = 40 }\n\
suspend fun call(b: Bar, f: suspend Bar.(Int) -> Int): Int = b.f(2)\n\
fun box(): String {\n\
    var res = 0\n\
    val block: suspend () -> Int = { call(Bar()) { n -> pause(); v + n } }\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    if (res != 0) return \"fail: completed before resume (res=$res, order=$order)\"\n\
    order += \"resuming;\"\n\
    saved!!.resume(Unit)\n\
    if (res != 42) return \"fail: after resume res=$res order=$order\"\n\
    if (order != \"parked;resuming;\") return \"fail order: $order\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("suspend receiver fn parks/resumes"), "OK");
}

#[test]
fn suspend_receiver_fn_safe_call_invoke() {
    // The `?.` form on a nullable receiver with a `suspend Bar.() -> Int` value: the suspension
    // point sits inside the null-check arm; the null receiver must skip the invoke entirely.
    const SRC: &str = "import kotlin.coroutines.*\n\
class Bar { val v = 41 }\n\
suspend fun call(b: Bar?, f: suspend Bar.() -> Int): Int = b?.f() ?: -1\n\
fun box(): String {\n\
    var res = 0\n\
    var nres = 0\n\
    val block: suspend () -> Unit = {\n\
        res = call(Bar()) { v + 1 }\n\
        nres = call(null) { v + 1 }\n\
    }\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    if (res != 42) return \"FAIL 1: $res\"\n\
    if (nres != -1) return \"FAIL 2: $nres\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("suspend safe-call receiver fn invoke"),
        "OK"
    );
}

#[test]
fn real_member_still_wins_over_scope_value() {
    // `Bar` HAS a member `f` — member resolution must win over the same-named scope value.
    const SRC: &str = "class Bar { fun f(): Int = 1 }\n\
fun call(b: Bar, f: Bar.() -> Int): Int = b.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { 2 }\n\
    return if (r == 1) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("member wins"), "OK");
}

#[test]
fn bare_receiver_function_uses_implicit_receiver() {
    const SRC: &str = r#"
var result = ""

class Scope {
    fun append(value: String) {
        result += value
    }

    fun apply(action: Scope.() -> Unit) {
        action()
    }
}

fun box(): String {
    Scope().apply { append("OK") }
    return result
}
"#;
    assert_eq!(run(SRC).expect("bare receiver function"), "OK");
}

#[test]
fn inferred_expression_preserves_receiver_function_shape() {
    const SRC: &str = r#"
var result = ""

class Scope {
    fun apply(enabled: Boolean, action: Scope.() -> Unit) {
        val alias = if (enabled) action else action as Scope.() -> Unit
        alias()
    }
}

fun box(): String {
    Scope().apply(true) { result = "OK" }
    return result
}
"#;
    assert_eq!(run(SRC).expect("receiver function expression"), "OK");
}

#[test]
fn top_level_receiver_function_uses_implicit_receiver() {
    const SRC: &str = r#"
var result = ""
class Scope

val action: Scope.() -> Unit = {
    result += "O"
}

fun Scope.applyAction() {
    action()
    val alias = action
    alias()
}

fun box(): String {
    Scope().applyAction()
    return if (result == "OO") "OK" else result
}
"#;
    assert_eq!(run(SRC).expect("top-level receiver function"), "OK");
}

#[test]
fn dispatch_property_origin_is_preserved() {
    const SRC: &str = r#"
var result = ""
class Target

val action: Target.() -> Unit = {
    result += "WRONG"
}

class Host(
    val action: Target.() -> Unit,
    val finish: Target.() -> Unit,
) {
    private fun Target.applyAction() {
        action()
        finish()
    }

    fun execute() {
        Target().applyAction()
    }
}

fun box(): String {
    Host(
        { result += "O" },
        { result += "K" },
    ).execute()
    return result
}
"#;
    assert_eq!(run(SRC).expect("dispatch property origin"), "OK");
}

#[test]
fn enclosing_and_interface_property_origins_are_preserved() {
    const SRC: &str = r#"
var result = ""
open class Target

class Container(val action: Target.() -> Unit) {
    inner class Item : Target() {
        fun applyAction() {
            action()
        }
    }
}

interface Scope {
    val finish: Scope.() -> Unit

    fun applyFinish() {
        finish()
    }
}

class Implementation(
    override val finish: Scope.() -> Unit,
) : Scope

fun box(): String {
    Container { result += "O" }.Item().applyAction()
    Implementation { result += "K" }.applyFinish()
    return result
}
"#;
    assert_eq!(run(SRC).expect("receiver function property origins"), "OK");
}

#[test]
fn cross_file_receiver_function_property_uses_implicit_receiver() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Action.kt",
            r#"
package sample

var result = ""
class Scope

val action: Scope.() -> Unit = {
    result = "OK"
}
"#,
        ),
        (
            "Main.kt",
            r#"
package sample

fun Scope.applyAction() {
    action()
}

fun box(): String {
    Scope().applyAction()
    return result
}
"#,
        ),
    ]);
    assert_eq!(output.expect("cross-file receiver function property"), "OK");
}

#[test]
fn nearest_compatible_implicit_receiver_is_selected() {
    const SRC: &str = r#"
var result = ""

open class Base
class Derived : Base()

class Host {
    fun execute(action: Base.() -> Unit) {
        Base().run {
            Derived().run {
                action()
            }
        }
    }
}

fun box(): String {
    Host().execute {
        result = if (this is Derived) "OK" else "WRONG"
    }
    return result
}
"#;
    assert_eq!(run(SRC).expect("implicit receiver tower"), "OK");
}

#[test]
fn ordinary_function_does_not_consume_implicit_receiver() {
    const SRC: &str = r#"
class Scope {
    fun call(function: (Scope, Int) -> String): String = function(1)
}
"#;
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("expects 2 args, got 1")),
        "expected ordinary function arity diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn unrelated_explicit_receiver_is_rejected() {
    const SRC: &str = r#"
class Expected
class Unrelated

fun call(value: Unrelated, action: Expected.() -> Unit) {
    value.action()
}
"#;
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unresolved reference 'action'")),
        "expected receiver mismatch diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn member_extension_accepts_unit_receiver_lambda() {
    const SRC: &str = r#"
class Element(val text: String)

class Scope {
    private fun StringBuilder.output(elements: Collection<Element>) {
        emit(elements) { element ->
            append(element.text)
            append("")
        }
    }

    private inline fun <E> StringBuilder.emit(
        elements: Collection<E>,
        render: StringBuilder.(E) -> Unit,
    ) {
        elements.forEach { element ->
            render(element)
        }
    }

    fun result(): String = buildString {
        output(listOf(Element("O"), Element("K")))
    }
}

fun box(): String = Scope().result()
"#;
    assert_eq!(run(SRC).expect("Unit receiver lambda"), "OK");
}

#[test]
fn member_extension_precedes_dispatch_receiver_function_property() {
    const SRC: &str = r#"
class A {
    fun test1(): Boolean {
        val foo: String.() -> Boolean = { false }
        fun String.foo(): Boolean = true
        return "1".foo()
    }

    fun test2(): Boolean {
        val foo: String.() -> Boolean = { false }
        fun String.foo(): Boolean = true
        return with("2") { foo() }
    }
}

class B {
    val foo: String.() -> Boolean = { false }
    fun String.foo(): Boolean = true

    fun test3(): Boolean = "1".foo()
    fun test4(): Boolean = with("2") { foo() }
}

fun box(): String {
    if (!A().test1()) return "FAIL 1"
    if (A().test2()) return "FAIL 2"
    if (!B().test3()) return "FAIL 3"
    if (!B().test4()) return "FAIL 4"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("receiver function precedence"), "OK");
}
