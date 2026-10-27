//! A same-module inline call whose literal lambda leaves its body non-locally (a `return` of the
//! enclosing function, or a `break` or `continue` of a loop around the call) is inlined from the
//! callee's compiled body, as kotlinc compiles it: the lambda's node ends each such jump with a
//! marked return the inliner turns into the caller's own jump.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const DECLARATION: &str = r#"inline fun foo(block: () -> Unit) { block() }

inline fun value(block: () -> Int): Int = block()

inline fun run2(block: () -> Unit) = block()
"#;

const CALLER: &str = r#"class A {
    val prop: Int
    constructor(arg: Boolean) {
        if (arg) {
            prop = 1
            run2 { return }
            throw RuntimeException("fail 0")
        }
        prop = 2
    }
}

fun first(values: List<Int>): Int {
    for (v in values) {
        foo { if (v > 1) return v }
    }
    return -1
}

fun box(): String {
    while (true) {
        foo { break }
        return "FAIL"
    }
    var sum = 0
    for (i in 1..10) {
        sum += value { if (i == 3) break else i }
    }
    if (sum != 3) return "sum $sum"
    if (first(listOf(1, 2, 3)) != 2) return "first"
    if (A(true).prop != 1 || A(false).prop != 2) return "prop"
    return "OK"
}
"#;

#[test]
fn a_lambda_that_leaves_non_locally_is_inlined_from_the_compiled_body_like_kotlinc() {
    assert_like_kotlinc(&[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)]);
}

#[test]
fn a_lambda_that_leaves_non_locally_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "CallerKt",
    );
}

/// A value class's inline member is its static `-impl`, whose first operand is the receiver: the
/// lambda's `crossinline`/`noinline` fact moves with it to the next position.
const VALUE_CLASS_MEMBER: &str = r#"@JvmInline
value class Token(val value: String) {
    inline fun inspect(block: (String) -> String): String = block(value)
}

fun choose(): String {
    Token("OK").inspect { return it }
    return "FAIL"
}

fun box(): String = Token(choose()).inspect { it }
"#;

#[test]
fn a_value_class_members_lambda_is_inlined_from_the_compiled_body_like_kotlinc() {
    assert_like_kotlinc(&[("ValueClassMember.kt", VALUE_CLASS_MEMBER)]);
}

#[test]
fn a_value_class_members_lambda_inlined_from_the_compiled_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(VALUE_CLASS_MEMBER, "ValueClassMember"),
        "OK"
    );
}

/// A `continue` the loop body runs on after. The bytes are not compared: the MethodInliner port
/// threads the lambda's conditional exit straight to the loop where kotlinc keeps a `goto` behind
/// it, the same against a library's compiled body.
const CONTINUE_CALLER: &str = r#"fun box(): String {
    var odd = 0
    for (i in 1..5) {
        foo { if (i % 2 == 0) continue }
        odd += i
    }
    return if (odd == 9) "OK" else "odd $odd"
}
"#;

#[test]
fn a_lambda_that_continues_a_loop_with_more_to_run_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", DECLARATION),
            ("Caller.kt", CONTINUE_CALLER),
        ],
        "CallerKt",
    );
}

/// A `tailrec` self-call returned non-locally from an inline lambda is a tail call of the
/// function: it loops, writing the parameters the lambda captures, rather than recursing a
/// million frames deep.
const TAILREC: &str = r#"private inline fun act(action: () -> Unit) {
    action()
}

class Counter {
    private inline fun step(action: () -> Unit) {
        return action()
    }

    private var countDown = 1000000

    tailrec fun test(): Int {
        if (countDown < 5) return countDown
        step {
            countDown--
            if (countDown < 1) return countDown else return test()
        }
        return countDown
    }
}

tailrec fun sum(n: Long, acc: Long): Long {
    if (n == 0L) return acc
    act { return sum(n - 1, acc + n) }
    return -1L
}

tailrec fun nested(n: Int, acc: Int): Int {
    if (n == 0) return acc
    act { act { return nested(n - 1, acc + 1) } }
    return -1
}

fun box(): String {
    if (Counter().test() != 4) return "test"
    if (sum(1000000L, 0L) != 500000500000L) return "sum"
    if (nested(1000000, 0) != 1000000) return "nested"
    return "OK"
}
"#;

#[test]
fn a_tailrec_self_call_returned_from_an_inline_lambda_loops() {
    assert_eq!(common::expect_box_run_with_stdlib(TAILREC, "Tailrec"), "OK");
}

/// A lambda that reads only some of the parameters its tail call writes. kotlinc's tail-recursion
/// lowering runs before its lambdas capture anything, so the step's write of `result` makes the
/// lambda capture it; the lambda here gains that capture, and the step writes it through the
/// function's own slot. Each function returns the value only the last step writes, a million deep.
/// kotlinc keeps each parameter in a variable of its own across the loop, so the bytes differ and
/// only the run is compared.
const UNCAPTURED_TAILREC: &str = r#"private inline fun act(block: () -> Unit) = block()

tailrec fun count(n: Int, result: Int): Int {
    if (n == 0) return result
    act { return count(n - 1, 7) }
    return -1
}

tailrec fun nested(n: Int, result: Int, tag: String): Int {
    if (n == 0) return result + tag.length
    act { act { return nested(n - 1, 7, "x") } }
    return -1
}

class Counter(val step: Int) {
    tailrec fun count(n: Int, result: Int): Int {
        if (n == 0) return result
        act { return count(n - step, 3) }
        return -1
    }
}

fun box(): String {
    if (count(1_000_000, 0) != 7) return "count"
    if (nested(1_000_000, 0, "") != 8) return "nested"
    if (Counter(1).count(1_000_000, 0) != 3) return "member"
    return "OK"
}
"#;

#[test]
fn a_tailrec_step_writes_a_parameter_its_inline_lambda_does_not_read() {
    assert_eq!(
        common::expect_box_run_with_stdlib(UNCAPTURED_TAILREC, "UncapturedTailrec"),
        "OK"
    );
}

/// The capture a step adds sits between the lambda's captures and its own parameters, which the
/// value-class lowering tells apart: `total`, a value class the lambda does not read, becomes a
/// capture, while the lambda's own `step` stays its own boxed parameter. kotlinc's result is the
/// reference; each function runs a million deep.
const VALUE_CLASS_TAILREC: &str = r#"@JvmInline
value class Meters(val value: Int)

private inline fun act(block: (Meters) -> Unit) = block(Meters(1))

tailrec fun walk(n: Int, total: Meters): Int {
    if (n == 0) return total.value
    act { step -> return walk(n - step.value, Meters(step.value + 6)) }
    return -1
}

tailrec fun pace(n: Meters, total: Meters): Int {
    if (n.value == 0) return total.value
    act { step -> act { return pace(Meters(n.value - step.value), Meters(it.value + 8)) } }
    return -1
}

fun box(): String {
    if (walk(1_000_000, Meters(0)) != 7) return "walk ${walk(1_000_000, Meters(0))}"
    if (pace(Meters(1_000_000), Meters(0)) != 9) return "pace"
    return "OK"
}
"#;

#[test]
fn a_tailrec_step_captures_a_value_class_beside_the_lambdas_own_value_class_parameter() {
    common::expect_box_same_as_kotlinc(VALUE_CLASS_TAILREC, "ValueClassTailrec");
}
