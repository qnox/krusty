//! A `suspend operator fun` declared in a SIBLING file and reached through its CONVENTION (`b[i]`,
//! `b[i] = v`, `b + 1`, `b += 1`, `-b`) rather than by name. Such a call has no `Expr::Call` node: the
//! checker keys its selected target to the `Expr::Index`/binary node, or to the assignment STATEMENT.
//! Every suspension scan around it keys on a call SHAPE, so the driving `suspend { … }` lambda used to
//! be classified as leaf — no state machine, and the call emitted with the pre-CPS descriptor and no
//! `Continuation`. The resulting `NoSuchMethodError` is swallowed by the driving continuation, so the
//! box answered `"fail"` rather than failing loudly.

use super::common;

/// Declares one `suspend operator` per convention under test. `Box.v` is read back after the coroutine
/// runs, so an assignment the machine dropped is observable.
const LIB: &str = "class Box(var v: Int)\n\
                   suspend operator fun Box.get(i: Int): Int = v + i\n\
                   suspend operator fun Box.set(i: Int, x: Int) { v = x + i }\n\
                   suspend operator fun Box.plus(i: Int): Box = Box(v + i)\n\
                   suspend operator fun Box.plusAssign(i: Int) { v += i }\n\
                   suspend operator fun Box.unaryMinus(): Box = Box(-v)\n";

/// `body` runs inside a `suspend { … }` driven by a `Continuation` that SWALLOWS failures, so a call
/// emitted without a continuation shows up as the unchanged `r`, never as an exception.
fn main_driving(body: &str) -> String {
    format!(
        "import kotlin.coroutines.*\n\
         class EC : Continuation<Unit> {{\n\
         \x20   override val context: CoroutineContext = EmptyCoroutineContext\n\
         \x20   override fun resumeWith(result: Result<Unit>) {{}}\n\
         }}\n\
         fun box(): String {{\n\
         \x20   var r = 0\n\
         \x20   val b = Box(1)\n\
         \x20   suspend {{ {body} }}.startCoroutine(EC())\n\
         \x20   return if (r == 2) \"OK\" else \"fail: $r\"\n\
         }}\n"
    )
}

fn expect_ok(body: &str, stem: &str) {
    let main = main_driving(body);
    common::expect_box_ok_files_with_stdlib(&[("Lib.kt", LIB), ("Main.kt", main.as_str())], stem);
}

/// Indexed READ. Its target is recorded in `resolved_calls` against the `Expr::Index` node, not in the
/// operator map — the shape-free lookup over every node is what finds it.
#[test]
fn suspend_indexed_get_cross_file_executes() {
    expect_ok("r = b[1]", "suspend_indexed_get_cross_file");
}

/// Indexed WRITE. Its `set` target is recorded against the assignment STATEMENT, so the scan has to
/// walk statements as well as expressions.
#[test]
fn suspend_indexed_set_cross_file_executes() {
    expect_ok("b[1] = 1; r = b.v", "suspend_indexed_set_cross_file");
}

/// Binary `+` desugared to a `suspend operator fun plus`.
#[test]
fn suspend_binary_plus_cross_file_executes() {
    expect_ok("r = (b + 1).v", "suspend_binary_plus_cross_file");
}

/// `b += 1` selects `plusAssign`, whose target lands in `StmtLowering::PlusAssign` rather than either
/// operator map — the one convention that needed the flag threaded through
/// `CompoundAssignmentTarget` as well.
#[test]
fn suspend_plus_assign_cross_file_executes() {
    expect_ok("b += 1; r = b.v", "suspend_plus_assign_cross_file");
}

/// A UNARY convention, to pin that the fix is not specific to a binary/indexed shape.
#[test]
fn suspend_unary_minus_cross_file_executes() {
    expect_ok("r = (-b).v + 3", "suspend_unary_minus_cross_file");
}
