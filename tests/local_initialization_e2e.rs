//! kotlinc's definite assignment of deferred locals (`val x: Int` / `var x: Int` assigned later):
//! a read before every path initializes the local, a `val` write after some path may have
//! initialized it, and a `val` write from a lambda or function that is not called in place. A
//! lambda argument runs in place by its callee's `callsInPlace` contract, or as an inline
//! parameter an unknown number of times.

use super::common;

const INVOKERS: &str = "@file:OptIn(ExperimentalContracts::class)
import kotlin.contracts.*

inline fun once(f: () -> Unit) { contract { callsInPlace(f, InvocationKind.EXACTLY_ONCE) }; f() }
inline fun atMost(f: () -> Unit) { contract { callsInPlace(f, InvocationKind.AT_MOST_ONCE) }; f() }
inline fun atLeast(f: () -> Unit) { contract { callsInPlace(f, InvocationKind.AT_LEAST_ONCE) }; f() }
inline fun unknown(f: () -> Unit) { contract { callsInPlace(f) }; f() }
inline fun inlined(f: () -> Unit) { f() }
fun later(f: () -> Unit) { f() }
";

fn source(body: &str) -> String {
    format!("{INVOKERS}\n{body}")
}

#[test]
fn a_lambda_initializes_by_its_invocation_kind() {
    let src = source(
        "fun exactlyOnce(): Int { val x: Int; once { x = 1 }; return x }
fun atMostOnce(): Int { val x: Int; atMost { x = 1 }; return x }
fun atLeastOnce(): Int { val x: Int; atLeast { x = 1 }; return x }
fun unknownTimes(): Int { val x: Int; unknown { x = 1 }; return x }
fun notInPlace(): Int { val x: Int; later { x = 1 }; return x }
fun varAtLeastOnce(): Int { var x: Int; atLeast { x = 1 }; return x }
fun varUnknownTimes(): Int { var x: Int; unknown { x = 1 }; return x }
fun varAtMostOnce(): Int { var x: Int; atMost { x = 1 }; return x }
fun inlineWithoutContract(): Int { val x: Int; inlined { x = 1 }; return x }
fun forEachRepeats(l: List<Int>): Int { val x: Int; l.forEach { x = it }; return x }
fun runIsExactlyOnce(): Int { val z: Int; run { z = 1 }; return z }
fun onceThenAssign() { val x: Int; once { x = 1 }; x = 2 }
fun assignThenOnce(): Int { val x: Int; x = 1; once { x = 2 }; return x }
fun returnInOnce(): Int { val x: Int; once { return 0 }; return x }
fun guardedOnce(b: Boolean): Int { val x: Int; once { if (b) return 1; x = 2 }; return x }
fun nestedDeclaration(): Int { val x: Int; once { val y: Int; y = 1; x = y }; return x }
",
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &[]);
}

#[test]
fn branches_and_jumps_decide_initialization() {
    let src = "enum class E { A, B }

fun oneBranch(b: Boolean): Int { val x: Int; if (b) x = 1; return x }
fun bothBranches(b: Boolean): Int { val x: Int; if (b) x = 1 else x = 2; return x }
fun returningBranch(b: Boolean): Int { val x: Int; if (b) { x = 1 } else { return 0 }; return x }
fun throwingElse(b: Boolean): Int { val x: Int; when { b -> x = 1; else -> throw Exception() }; return x }
fun nothingCall(b: Boolean): Int { val x: Int; if (b) x = 1 else error(\"no\"); return x }
fun exhaustiveBoolean(b: Boolean): Int { val x: Int; when (b) { true -> x = 1; false -> x = 2 }; return x }
fun exhaustiveEnum(e: E): Int { val x: Int; when (e) { E.A -> x = 1; E.B -> x = 2 }; return x }
fun twoIfs(b: Boolean): Int {
    val x: Int
    if (b) { x = 1 }
    if (!b) { x = 2 }
    return x
}
fun shortCircuit(b: Boolean): Int { val x: Int; b && run { x = 1; true }; return x }
fun valueBlock(): Int { val x: Int; val y = run { x = 1; x }; return y }
fun ifValue(b: Boolean): Int { val x: Int; x = if (b) 1 else 2; return x }
fun elvisReturn(s: String?): Int { val x: Int; s ?: run { x = 1; return x }; return 2 }
fun twice() { val y: Int; y = 1; y = 2 }
fun varTwice(): Int { var x: Int; x = 1; x = 2; return x }
fun compound(): Int { val x: Int; x += 1; return x }
fun varIncrement(): Int { var x: Int; x++; return x }
fun selfRead(): Int { var x: Int; x = x + 1; return x }
fun incrementInitialized(): Int { val x: Int; x = 1; x++; return x }
fun twoLocals(): Int { val x: Int; val y: Int; x = 1; return x + y }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

#[test]
fn loops_and_try_repeat_or_interrupt_writes() {
    let src = "fun whileBody(b: Boolean): Int { val x: Int; while (b) { x = 1 }; return x }
fun forBody(): Int { var x: Int; for (i in 0..1) x = i; return x }
fun doOnce(): Int { val x: Int; do { x = 1 } while (false); return x }
fun doRepeated(b: Boolean): Int { val x: Int; do { x = 1 } while (b); return x }
fun whileTrueBreak(b: Boolean): Int { val x: Int; while (true) { if (b) { x = 1; break } }; return x }
fun breakAfterWrite(b: Boolean): Int { val x: Int; while (b) { x = 1; break }; return x }
fun whileTrueRepeats(): Int { val x: Int; while (true) { x = 1 } }
fun whileFalse(): Int { val x: Int; while (false) { x = 1 }; return x }
fun labeledBreak(b: Boolean): Int {
    val x: Int
    outer@ for (i in 0..2) { while (b) { x = 1; break@outer } }
    return x
}
fun continueSkips(b: Boolean): Int { val x: Int; for (i in 0..2) { if (b) continue; x = 1 }; return x }
fun varPerIteration(): Int { var x: Int; for (i in 0..2) { x = i; println(x) }; return 0 }
fun catchReassigns(): Int { val x: Int; try { x = 1 } catch (e: Exception) { x = 2 }; return x }
fun emptyFinally(): Int { val x: Int; try { x = 1 } finally { }; return x }
fun finallyReassigns(): Int { val x: Int; try { x = 1 } finally { x = 2 }; return x }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

#[test]
fn lambdas_and_local_functions_see_the_declaration_point() {
    let src = "fun lambdaReadsEarly() { val x: Int; val f = { x }; x = 1 }
fun varLambdaReadsEarly(): Int { var x: Int; val f = { x }; x = 1; return f() }
fun lambdaReadsLate(): Int { val x: Int; x = 1; val g = { x }; return g() }
fun lambdaWritesVar(): Int { var x: Int; x = 1; val g = { x = 2 }; g(); return x }
fun localFunctionWritesVal(): Int { val x: Int; fun g() { x = 1 }; g(); return x }
fun localFunctionReadsEarly(): Int {
    val x: Int
    fun g(): Int { return x }
    x = 1
    return g()
}
fun localFunctionWritesVar(): Int {
    var x: Int
    fun g() { x = 1 }
    g()
    x = 2
    return x
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Deferred locals that every path initializes once compile and run like kotlinc's.
#[test]
fn initialized_deferred_locals_are_accepted() {
    let src = "@file:OptIn(ExperimentalContracts::class)
import kotlin.contracts.*

inline fun once(f: () -> Unit) { contract { callsInPlace(f, InvocationKind.EXACTLY_ONCE) }; f() }

fun pick(b: Boolean): String {
    val x: String
    if (b) x = \"O\" else x = \"K\"
    return x
}

fun box(): String {
    val o: String
    once { o = pick(true) }
    val k: String
    when {
        o.isEmpty() -> throw IllegalStateException()
        else -> k = pick(false)
    }
    var counter: Int
    counter = 0
    for (i in 0..2) counter++
    val done: Boolean
    try { done = counter == 3 } finally { }
    return if (done) o + k else \"fail\"
}
";
    common::assert_sources_accepted_like_kotlinc(&[("main.kt", src)], &[]);
}

/// The arguments of a call are evaluated, in source order, before the callee runs a lambda in
/// place: a later argument reads the local before the lambda assigns it, whatever the parameter
/// order.
#[test]
fn an_in_place_lambda_runs_after_every_argument() {
    let src = "@file:OptIn(ExperimentalContracts::class)
import kotlin.contracts.*

fun invoke(block: () -> Unit, value: Int): Int {
    contract { callsInPlace(block, InvocationKind.EXACTLY_ONCE) }
    block()
    return value
}
fun invokeAfter(value: Int, block: () -> Unit): Int {
    contract { callsInPlace(block, InvocationKind.EXACTLY_ONCE) }
    block()
    return value
}
fun lambdaFirst(): Int { val x: Int; return invoke({ x = 1 }, x) }
fun namedLambdaFirst(): Int { val x: Int; return invokeAfter(block = { x = 1 }, value = x) }
fun lambdaLast(): Int { val x: Int; invokeAfter(0, { x = 1 }); return x }
fun trailingLambda(): Int { val x: Int; invokeAfter(0) { x = 1 }; return x }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Property accessors and `init` blocks are executable bodies of their own, checked like a
/// function's.
#[test]
fn accessors_and_init_blocks_check_their_deferred_locals() {
    let src = "val getterRead: Int
    get() {
        val x: Int
        return x
    }
val expressionGetter: Int get() = run { val x: Int; x }
var setterRead: Int = 0
    set(value) {
        val x: Int
        field = x + value
    }
class Members {
    val memberGetter: Int
        get() {
            val x: Int
            return x
        }
    var memberSetter: Int = 0
        set(value) {
            val y: Int
            y = value
            y = 2
            field = y
        }
    init {
        val x: Int
        println(x)
    }
    init {
        val y: Int
        y = 1
        y = 2
    }
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Every independently evaluated declaration expression is a body-check root. In particular,
/// property values and constructor parameter defaults do not have a surrounding function body
/// whose flow pass could discover their deferred locals later. Ordinary function defaults remain
/// in the fixture because kotlinc deliberately omits this diagnostic there; the exact ledger also
/// protects that distinction.
#[test]
fn declaration_expression_roots_check_their_deferred_locals() {
    let src = "val topInitializer: Int = run { val x: Int; x }
val topDelegate: Int by run { val delegate: Lazy<Int>; delegate }

fun defaulted(value: Int = run { val x: Int; x }) = value
fun inferredBody() = run { val x: Int; x }

class PrimaryDefault(value: Int = run { val x: Int; x })

class Members {
    val initializer: Int = run { val x: Int; x }
    val delegated: Int by run { val delegate: Lazy<Int>; delegate }
    fun defaulted(value: Int = run { val x: Int; x }) = value
    fun inferredBody() = run { val x: Int; x }
}

class SecondaryBody {
    constructor() {
        val x: Int
        println(x)
    }
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Constructor and delegation operands are executable roots too: they run outside every method or
/// `init` block, but still own the complete control flow of locals declared inside the operand.
#[test]
fn constructor_and_delegation_roots_check_their_deferred_locals() {
    let src = "open class Base(val value: Int)
interface Face
fun face(): Face = object : Face {}
fun useFace(value: Face): Face = value

class PrimaryHeader : Base(run { val x: Int; x })
class InterfaceDelegate : Face by useFace(run { val face: Face; face })
class SecondaryHeader : Base {
    constructor() : super(run { val x: Int; x })
}
class SecondaryDefault : Base {
    constructor(value: Int = run { val x: Int; x }) : super(value)
}
enum class Choice(val value: Int) {
    ENTRY(run { val x: Int; x })
}

fun anonymousHeader() {
    val assigned: Int
    val value = object : Base(run { val missing: Int; missing }),
        Face by useFace(run { assigned = 1; face() }) {}
    println(assigned)
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A script body is one executable root even though it has no enclosing function declaration.
#[test]
fn script_roots_check_their_deferred_locals() {
    let source = "run { val x: Int; println(x) }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let (krusty_code, krusty_stderr) = common::front_end_named_input_result(
        "Deferred.kts",
        krusty::frontend::SourceInput::kotlin_script(source).with_file_stem("Deferred"),
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    let (reference_code, reference_stderr) =
        common::kotlinc_named_source_result("Deferred.kts", source);
    assert_eq!(
        (krusty_code, common::compiler_errors(&krusty_stderr)),
        (reference_code, common::compiler_errors(&reference_stderr)),
        "\nkrusty: {krusty_stderr}\nkotlinc: {reference_stderr}"
    );
}
