//! Suspend-function `try`/`catch` shapes from production controller code.
//!
//! Three related gaps in the state-machine flattener:
//! 1. A value-position `try` under a RESULT COERCION (`suspend fun f(): Base = try { sub() } catch
//!    (…) { other() }` — the try's value is cast to the declared return type) was not desugared:
//!    `desugar_value_try` matched a bare `Return(Try)` only, so the cast-wrapped form fell through
//!    to the flattener's unmodeled-`Variable` bail and the file skipped.
//! 2. Multiple `catch` clauses skipped the file (`catches.len() != 1`), though catch bodies that do
//!    not suspend all emit inside the one handler state.
//! 3. MISCOMPILE: the handler state ran its catch body for EVERY `Throwable` routed to it — no
//!    exception-type test at all. `catch (e: Miss)` swallowed an `IllegalStateException` the caller
//!    should have seen (kotlinc: a non-matching exception propagates out of the coroutine).
use super::common;

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    common::compile_and_run_box(main, tag, &[sl, coro, jdk.clone()], Some(jdk.as_path()))
}

#[test]
fn nonmatching_exception_propagates_past_suspend_catch() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class Miss : RuntimeException()\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"ok\")\n\
        \x20\x20 throw IllegalStateException(\"boom-\" + v)\n\
        \x20 } catch (e: Miss) {\n\
        \x20\x20 \"caught-miss\"\n\
        \x20 }\n\
        }\n\
        fun box(): String = try {\n\
        \x20 runBlocking { f() }\n\
        } catch (e: IllegalStateException) {\n\
        \x20 \"ise:\" + e.message\n\
        } catch (e: Throwable) {\n\
        \x20 \"wrong:\" + e.javaClass.simpleName\n\
        }\n";
    assert_eq!(
        run("Main", MAIN).expect("non-matching exception shape"),
        "ise:boom-ok",
        "a non-matching exception must propagate, not run the catch body"
    );
}

#[test]
fn matching_exception_still_caught() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class Hit(msg: String) : RuntimeException(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"ok\")\n\
        \x20\x20 throw Hit(\"h-\" + v)\n\
        \x20 } catch (e: Hit) {\n\
        \x20\x20 \"caught:\" + e.message\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking { f() }\n";
    assert_eq!(
        run("Main", MAIN).expect("matching exception shape"),
        "caught:h-ok"
    );
}

#[test]
fn multi_catch_selects_by_exception_type() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class A(msg: String) : RuntimeException(msg)\n\
        class B(msg: String) : RuntimeException(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(which: Int): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"v\")\n\
        \x20\x20 when (which) {\n\
        \x20\x20\x20 0 -> v\n\
        \x20\x20\x20 1 -> throw A(\"a-\" + v)\n\
        \x20\x20\x20 else -> throw B(\"b-\" + v)\n\
        \x20\x20 }\n\
        \x20 } catch (e: A) {\n\
        \x20\x20 \"A:\" + e.message\n\
        \x20 } catch (e: B) {\n\
        \x20\x20 \"B:\" + e.message\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking {\n\
        \x20 f(0) + \"/\" + f(1) + \"/\" + f(2)\n\
        }\n";
    assert_eq!(
        run("Main", MAIN).expect("multi-catch shape"),
        "v/A:a-v/B:b-v"
    );
}

#[test]
fn expression_bodied_try_with_result_coercion() {
    // The try's value (`Sub`) coerces to the declared return type (`Base`), wrapping the
    // value-position `Try` in a cast the desugar must see through.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        open class Base(val tag: String)\n\
        class Sub(tag: String) : Base(tag)\n\
        class Boom(msg: String) : RuntimeException(msg)\n\
        suspend fun mk(fail: Boolean): Sub =\n\
        \x20 if (fail) throw Boom(\"x\") else Sub(\"sub\")\n\
        suspend fun f(fail: Boolean): Base = try {\n\
        \x20 Sub(mk(fail).tag + \"!\")\n\
        } catch (e: Boom) {\n\
        \x20 Base(\"fell-\" + e.message)\n\
        }\n\
        fun box(): String = runBlocking {\n\
        \x20 f(false).tag + \"/\" + f(true).tag\n\
        }\n";
    assert_eq!(
        run("Main", MAIN).expect("cast-wrapped value try"),
        "sub!/fell-x"
    );
}

#[test]
fn expression_bodied_multi_catch_maps_exceptions() {
    // The production controller shape: an expression-bodied suspend function whose whole body is a
    // `try` with several non-suspending catches mapping exceptions to a common supertype.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        open class R(val s: String)\n\
        class Ok(s: String) : R(s)\n\
        class E1(msg: String) : RuntimeException(msg)\n\
        class E2(msg: String) : RuntimeException(msg)\n\
        suspend fun step(which: Int): String = when (which) {\n\
        \x20 0 -> \"fine\"\n\
        \x20 1 -> throw E1(\"one\")\n\
        \x20 else -> throw E2(\"two\")\n\
        }\n\
        suspend fun f(which: Int): R = try {\n\
        \x20 Ok(step(which))\n\
        } catch (e: E1) {\n\
        \x20 R(\"e1:\" + e.message)\n\
        } catch (e: E2) {\n\
        \x20 R(\"e2:\" + e.message)\n\
        }\n\
        fun box(): String = runBlocking {\n\
        \x20 f(0).s + \"/\" + f(1).s + \"/\" + f(2).s\n\
        }\n";
    assert_eq!(
        run("Main", MAIN).expect("expression-bodied multi-catch"),
        "fine/e1:one/e2:two"
    );
}

#[test]
fn suspending_catch_body_type_filter() {
    // The catch body itself suspends; a non-matching exception must still propagate rather than
    // enter the handler (previously: unconditional entry, then a ClassCastException at the bind).
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class Miss(msg: String) : RuntimeException(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(fail: Boolean): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"ok\")\n\
        \x20\x20 if (fail) throw IllegalStateException(\"boom\")\n\
        \x20\x20 v\n\
        \x20 } catch (e: Miss) {\n\
        \x20\x20 s(\"recovered-\" + e.message)\n\
        \x20 }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a = runBlocking { f(false) }\n\
        \x20 val b = try {\n\
        \x20\x20 runBlocking { f(true) }\n\
        \x20 } catch (e: IllegalStateException) {\n\
        \x20\x20 \"ise\"\n\
        \x20 } catch (e: Throwable) {\n\
        \x20\x20 \"wrong:\" + e.javaClass.simpleName\n\
        \x20 }\n\
        \x20 return a + \"/\" + b\n\
        }\n";
    assert_eq!(
        run("Main", MAIN).expect("suspending catch filter"),
        "ok/ise"
    );
}

#[test]
fn suspending_catch_body_still_recovers_on_match() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class Hit(msg: String) : RuntimeException(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"ok\")\n\
        \x20\x20 throw Hit(\"h\")\n\
        \x20 } catch (e: Hit) {\n\
        \x20\x20 s(\"recovered-\" + e.message)\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking { f() }\n";
    assert_eq!(
        run("Main", MAIN).expect("suspending catch recover"),
        "recovered-h"
    );
}

#[test]
fn catch_throwable_still_catches_everything() {
    // A `Throwable` catch is full-coverage: no type guard, exact prior behavior.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"ok\")\n\
        \x20\x20 throw IllegalStateException(\"any-\" + v)\n\
        \x20 } catch (e: Throwable) {\n\
        \x20\x20 \"all:\" + e.message\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking { f() }\n";
    assert_eq!(run("Main", MAIN).expect("Throwable catch"), "all:any-ok");
}

#[test]
fn bound_val_try_runs_and_filters() {
    // The locally-BOUND value-try form (`val x = try { … } catch { … }`) — previously a rejected
    // shape (`suspend_try_as_expression_rejected`), now desugared onto the bound local. Both the
    // caught path and the non-matching-propagation path must behave.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class Miss(msg: String) : RuntimeException(msg)\n\
        suspend fun d(which: Int): Int = when (which) {\n\
        \x20 0 -> 1\n\
        \x20 1 -> throw Miss(\"m\")\n\
        \x20 else -> throw IllegalStateException(\"boom\")\n\
        }\n\
        suspend fun f(which: Int): Int {\n\
        \x20 val x = try { d(which) } catch (e: Miss) { 0 }\n\
        \x20 return x + 10\n\
        }\n\
        fun box(): String {\n\
        \x20 val a = runBlocking { f(0) }\n\
        \x20 val b = runBlocking { f(1) }\n\
        \x20 val c = try {\n\
        \x20\x20 runBlocking { f(2) }.toString()\n\
        \x20 } catch (e: IllegalStateException) {\n\
        \x20\x20 \"ise\"\n\
        \x20 }\n\
        \x20 return a.toString() + \"/\" + b + \"/\" + c\n\
        }\n";
    assert_eq!(run("Main", MAIN).expect("bound val try"), "11/10/ise");
}

#[test]
fn multi_catch_subtype_ordering_first_match_wins() {
    // Source order selects the FIRST matching clause: a `SubEx` thrown with `catch (e: SubEx)`
    // BEFORE `catch (e: SupEx)` must take the first arm even though both match.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        open class SupEx(msg: String) : RuntimeException(msg)\n\
        class SubEx(msg: String) : SupEx(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(sub: Boolean): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"v\")\n\
        \x20\x20 if (sub) throw SubEx(\"s-\" + v) else throw SupEx(\"p-\" + v)\n\
        \x20 } catch (e: SubEx) {\n\
        \x20\x20 \"sub:\" + e.message\n\
        \x20 } catch (e: SupEx) {\n\
        \x20\x20 \"sup:\" + e.message\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking { f(true) + \"/\" + f(false) }\n";
    assert_eq!(
        run("Main", MAIN).expect("subtype-ordered multi-catch"),
        "sub:s-v/sup:p-v"
    );
}

#[test]
fn multi_catch_with_trailing_throwable_arm() {
    // A guarded arm followed by a full-coverage `Throwable` arm: the trailing arm is unguarded
    // (no re-throw else), and both selection directions must behave.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        class A(msg: String) : RuntimeException(msg)\n\
        suspend fun s(k: String): String = k\n\
        suspend fun f(a: Boolean): String {\n\
        \x20 return try {\n\
        \x20\x20 val v = s(\"v\")\n\
        \x20\x20 if (a) throw A(\"a-\" + v) else throw IllegalStateException(\"i-\" + v)\n\
        \x20 } catch (e: A) {\n\
        \x20\x20 \"A:\" + e.message\n\
        \x20 } catch (e: Throwable) {\n\
        \x20\x20 \"T:\" + e.message\n\
        \x20 }\n\
        }\n\
        fun box(): String = runBlocking { f(true) + \"/\" + f(false) }\n";
    assert_eq!(
        run("Main", MAIN).expect("trailing Throwable arm"),
        "A:a-v/T:i-v"
    );
}
