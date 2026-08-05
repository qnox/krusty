//! Kotlin evaluates call operands strictly left-to-right, and kotlinc spills EVERY operand of a
//! call that has a later suspending operand. `hoist_expr` rewrites `f(g(), susp())` by binding the
//! suspension to a preceding temp — if the earlier effectful operand `g()` stays inline in the
//! residual call, it runs AFTER the suspension, inverting the observable order. Each test pins the
//! g-before-susp order (and the arithmetic result, so the snapshot temp must survive a REAL
//! suspension via `yield()`). Needs the JVM toolchain + kotlin-stdlib + kotlinx-coroutines +
//! real kotlinc.
use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    common::compile_and_run_box(main, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()))
}

const PRELUDE: &str = "import kotlinx.coroutines.runBlocking\n\
import kotlinx.coroutines.yield\n\
var order = \"\"\n\
fun g(): Int { order += \"g\"; return 1 }\n\
suspend fun susp(): Int { order += \"s\"; yield(); return 2 }\n";

#[test]
fn call_arg_before_suspending_arg_runs_first() {
    // Top-level (IrExpr::Call) callee: f(g(), susp()) must observe g, then s, and f(1, 2) = 12.
    let src = format!(
        "{PRELUDE}\
fun f(a: Int, b: Int): Int = a * 10 + b\n\
suspend fun test(): String {{ val r = f(g(), susp()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"gs12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("call with suspending 2nd arg should compile + run");
    assert_eq!(
        out, "OK",
        "earlier effectful arg must evaluate before the hoisted suspension"
    );
}

#[test]
fn method_call_arg_before_suspending_arg_runs_first() {
    // Member (IrExpr::MethodCall) callee: c.m(g(), susp()) with the same ordering contract.
    let src = format!(
        "{PRELUDE}\
class C {{ fun m(a: Int, b: Int): Int = a * 10 + b }}\n\
suspend fun test(): String {{ val r = C().m(g(), susp()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"gs12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("method call with suspending 2nd arg should compile + run");
    assert_eq!(
        out, "OK",
        "earlier effectful arg must evaluate before the hoisted suspension"
    );
}

#[test]
fn method_call_effectful_receiver_before_suspending_arg() {
    // The RECEIVER is an operand too: mk().m(susp()) must run mk() before the suspension.
    let src = format!(
        "{PRELUDE}\
class C(val k: Int) {{ fun m(b: Int): Int = k * 10 + b }}\n\
fun mk(): C {{ order += \"m\"; return C(1) }}\n\
suspend fun test(): String {{ val r = mk().m(susp()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"ms12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("method call with effectful receiver should compile + run");
    assert_eq!(
        out, "OK",
        "receiver must evaluate before the hoisted suspension"
    );
}

#[test]
fn string_concat_part_before_suspending_part() {
    // Template (IrExpr::StringConcat) parts: "${g()}${susp()}" evaluates g then susp.
    let src = format!(
        "{PRELUDE}\
suspend fun test(): String {{ val s = \"${{g()}}${{susp()}}\"; return order + s }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"gs12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("string template with suspending part should compile + run");
    assert_eq!(
        out, "OK",
        "earlier template part must evaluate before the hoisted suspension"
    );
}

#[test]
fn toplevel_var_operand_reads_before_suspension_mutates_it() {
    // A top-level `var` read (GetStatic) is an OBSERVABLE operand: kotlinc spills it before the
    // suspension, so `f(c, bump())` reads the PRE-mutation value even though `bump()` writes `c`.
    let src = format!(
        "{PRELUDE}\
var c = 0\n\
fun f(a: Int, b: Int): Int = a * 10 + b\n\
suspend fun bump(): Int {{ c = 5; order += \"s\"; yield(); return 1 }}\n\
suspend fun test(): String {{ val r = f(c, bump()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"s1\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("call with a mutable top-level var operand should compile + run");
    assert_eq!(
        out, "OK",
        "a var static operand must be read before the suspension mutates it"
    );
}

#[test]
fn local_read_precedes_inline_block_mutation_and_suspension() {
    // Inline lowering can splice the later operand's block statements into the hoist prelude. The
    // earlier `GetValue` must therefore be snapshotted too: leaving it in the residual call changes
    // `f(x, run { x = 5; susp() })` into the equivalent of `x = 5; val t = susp(); f(x, t)` and reads
    // 5 instead of the source-order value 0. Conservatively materializing ordinary local/parameter
    // reads matches the shared ordered-operand rule and avoids a special scan for writes in one later
    // block shape.
    let src = format!(
        "{PRELUDE}\
fun f(a: Int, b: Int): Int = a * 10 + b\n\
suspend fun test(): String {{\n\
    var x = 0\n\
    val r = f(x, run {{ x = 5; susp() }})\n\
    return order + r\n\
}}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"s2\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out =
        run(&src).expect("local read before a mutating suspending block should compile + run");
    assert_eq!(
        out, "OK",
        "the local operand must be read before the later inline block mutates it"
    );
}

#[test]
fn notnull_assert_throws_before_suspension_effects() {
    // `x!!` THROWS at its evaluation position: in `f2(x!!, susp())` with a null `x`, kotlinc raises
    // the NPE before `susp()` runs — the suspension's side effect must never be observed.
    let src = format!(
        "{PRELUDE}\
fun maybe(): String? = null\n\
fun f2(a: String, b: Int): String = a + b\n\
suspend fun test(): String {{ val x = maybe(); return f2(x!!, susp()) }}\n\
fun box(): String = try {{\n\
    runBlocking {{ test() }}\n\
    \"ran:$order\"\n\
}} catch (e: NullPointerException) {{\n\
    if (order == \"\") \"OK\" else \"F:$order\"\n\
}}\n"
    );
    let out = run(&src).expect("call with a throwing !! operand should compile + run");
    assert_eq!(
        out, "OK",
        "the !! throw must precede the suspension's side effects"
    );
}

#[test]
fn effectful_operand_between_two_suspensions_keeps_order() {
    // `f3(susp1(), g(), susp2())`: g sits BETWEEN two suspension points and must run between them.
    let src = format!(
        "{PRELUDE}\
suspend fun s1(): Int {{ order += \"1\"; yield(); return 1 }}\n\
suspend fun s2(): Int {{ order += \"2\"; yield(); return 2 }}\n\
fun f3(a: Int, b: Int, c: Int): Int = a * 100 + b * 10 + c\n\
suspend fun test(): String {{ val r = f3(s1(), g(), s2()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"1g2112\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("call with an operand between two suspensions should compile + run");
    assert_eq!(
        out, "OK",
        "an effectful operand between two suspensions must keep its position"
    );
}

#[test]
fn property_receiver_before_suspending_arg_compiles() {
    // The dominant service/DI shape: a field/property-read RECEIVER with a suspending argument
    // (`h.svc.m(susp())`). The receiver snapshot needs a type for `PropertyRead`/`GetField`;
    // without one the whole file used to skip.
    let src = format!(
        "{PRELUDE}\
class Svc(val k: Int) {{ fun m(b: Int): Int = k * 10 + b }}\n\
class Holder {{ val svc = Svc(1) }}\n\
val h = Holder()\n\
suspend fun test(): String {{ val r = h.svc.m(susp()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"s12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("property receiver with suspending arg should compile + run");
    assert_eq!(
        out, "OK",
        "the property receiver must evaluate before the hoisted suspension"
    );
}

#[test]
fn suspending_call_own_args_keep_order() {
    // The suspension-point path itself: sf(g(), susp()) — the outer call suspends AND an arg
    // suspends; g still runs first.
    let src = format!(
        "{PRELUDE}\
suspend fun sf(a: Int, b: Int): Int {{ yield(); return a * 10 + b }}\n\
suspend fun test(): String {{ val r = sf(g(), susp()); return order + r }}\n\
fun box(): String = runBlocking {{\n\
    val o = test()\n\
    if (o == \"gs12\") \"OK\" else \"F:$o\"\n\
}}\n"
    );
    let out = run(&src).expect("suspend call with suspending 2nd arg should compile + run");
    assert_eq!(
        out, "OK",
        "earlier effectful arg must evaluate before the hoisted suspension"
    );
}
