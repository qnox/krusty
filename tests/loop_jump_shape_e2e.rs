//! How a loop reaches its own condition, and how a `break`/`continue` arm reaches the loop.
//!
//! Two jumps to a jump, which kotlinc does not write:
//!
//!   * a PRE-TEST loop with no update has nothing at the bottom but the back edge, so a `continue`
//!     that targeted the bottom arrived at the condition one hop late;
//!   * a branch whose body is nothing but `break`/`continue` was emitted as `if !cond -> next;
//!     goto target; next:` — a branch AROUND a jump, where one inverted branch does it.
//!
//! Neither changes what the program does, and both are in the shape every `while` with a guard
//! clause compiles to.
use super::common;

/// The shape that carries both: a guard clause that continues, in a loop with no update.
#[test]
fn a_continue_guard_is_byte_identical_to_kotlinc() {
    let src = "fun count(n: Int): Int {\n\
               \x20   var i = 0\n\
               \x20   var total = 0\n\
               \x20   while (i < n) {\n\
               \x20       i += 1\n\
               \x20       if (i % 2 == 0) continue\n\
               \x20       total += i\n\
               \x20   }\n\
               \x20   return total\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("ContinueGuard", src, "ContinueGuardKt")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("ContinueGuardKt byte-identical to kotlinc");
}

/// `break` takes the same path out of the `when`, to the loop's exit rather than its condition.
///
/// A loop WITH an update keeps its bottom — `continue` there must still run the update, so the
/// condition is not its target. That case is covered by running it, below, rather than by bytes:
/// krusty's counted-loop lowering spills the bound into a local where kotlinc reads the parameter,
/// which is a separate divergence this test would otherwise fail on.
#[test]
fn a_break_guard_is_byte_identical_to_kotlinc() {
    let src = "fun firstGap(n: Int): Int {\n\
               \x20   var seen = 0\n\
               \x20   var i = 0\n\
               \x20   while (i < n) {\n\
               \x20       i += 1\n\
               \x20       if (i == 3) break\n\
               \x20       seen += 1\n\
               \x20   }\n\
               \x20   return seen\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("BreakGuard", src, "BreakGuardKt") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("BreakGuardKt byte-identical to kotlinc");
}

/// The loops still run. A fused guard that inverted the wrong way, or a `continue` that skipped the
/// update, would keep the byte tests above honest only by accident.
#[test]
fn fused_guards_still_run_correctly() {
    let src = "fun box(): String {\n\
               \x20   var i = 0\n\
               \x20   var odd = 0\n\
               \x20   while (i < 10) {\n\
               \x20       i += 1\n\
               \x20       if (i % 2 == 0) continue\n\
               \x20       odd += i\n\
               \x20   }\n\
               \x20   var upto = 0\n\
               \x20   for (x in 1..10) {\n\
               \x20       if (x > 4) break\n\
               \x20       upto += x\n\
               \x20   }\n\
               \x20   return \"$odd $upto\"\n\
               }\n";
    let Some(actual) =
        common::compile_and_run_box(src, "fused_guards", &[common::stdlib_jar()], None)
    else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(
        actual, "25 10",
        "the guards must keep their original control flow"
    );
}

/// Constant guards exercise the reachability result returned by the fused conditional emitter.
/// A true guard must stop emission after its unconditional jump; a false guard must fall through.
#[test]
fn constant_fused_guards_preserve_reachability() {
    let src = "fun box(): String {\n\
               \x20   var continued = 0\n\
               \x20   var unreachable = 0\n\
               \x20   while (continued < 3) {\n\
               \x20       continued += 1\n\
               \x20       if (true) continue\n\
               \x20       unreachable += 1\n\
               \x20   }\n\
               \x20   var fellThrough = 0\n\
               \x20   while (fellThrough < 2) {\n\
               \x20       fellThrough += 1\n\
               \x20       if (false) continue\n\
               \x20       fellThrough += 1\n\
               \x20   }\n\
               \x20   var broke = 0\n\
               \x20   while (broke < 4) {\n\
               \x20       broke += 1\n\
               \x20       if (true) break\n\
               \x20   }\n\
               \x20   return \"$continued $unreachable $fellThrough $broke\"\n\
               }\n";
    let Some(actual) =
        common::compile_and_run_box(src, "constant_fused_guards", &[common::stdlib_jar()], None)
    else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(
        actual, "3 0 2 1",
        "constant guards must retain both unconditional and fall-through control flow"
    );
}

/// A `break` in a loop condition targets the enclosing loop. A `while` condition is outside that
/// loop, so `while (break)` leaves the outer loop before the body. A `do`/`while` condition is
/// inside that loop, so the body runs once and the outer loop continues. Official box
/// `controlStructures/breakContinueInExpressions/breakInLoopConditions.kt`.
#[test]
fn break_in_a_loop_condition_targets_the_enclosing_loop() {
    let src = "fun breakInDoWhileCondition(): String {\n\
var i = 0\n\
while (true) {\n\
++i\n\
var j = 0\n\
do {\n\
++j\n\
} while (break)\n\
if (j != 1) return \"FAIL1\"\n\
if (i == 3) break\n\
}\n\
if (i != 3) return \"FAIL2\"\n\
return \"OK\"\n\
}\n\
fun breakInWhileCondition(): String {\n\
var i = 0\n\
while (true) {\n\
++i\n\
var j = 0\n\
while (break) {\n\
j++\n\
}\n\
return \"FAIL3\"\n\
}\n\
if (i != 1) return \"FAIL4\"\n\
return \"OK\"\n\
}\n\
fun box(): String {\n\
val breakInDoWhileResult = breakInDoWhileCondition()\n\
if (breakInDoWhileResult != \"OK\") return breakInDoWhileResult\n\
return breakInWhileCondition()\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "breakInLoopConditions");
}

/// The pre-test loop is not in scope while its own condition is evaluated, so an unlabeled
/// `continue` there resumes the enclosing loop rather than targeting the loop being entered.
#[test]
fn continue_in_a_while_condition_targets_the_enclosing_loop() {
    let src = "fun box(): String {\n\
var iterations = 0\n\
while (++iterations < 3) {\n\
while (continue) {\n\
return \"FAIL-inner\"\n\
}\n\
return \"FAIL-outer\"\n\
}\n\
return if (iterations == 3) \"OK\" else \"FAIL-count: $iterations\"\n\
}\n";
    common::expect_box_same_as_kotlinc(src, "ContinueInWhileCondition");
}

/// A `do`/`while` condition remains inside that loop. Its unlabeled `continue` therefore targets
/// the condition itself; compare bytes without executing the intentional infinite loop.
#[test]
fn continue_in_a_do_while_condition_targets_that_loop() {
    let src = "fun spin() {\n\
do {\n\
} while (continue)\n\
}\n";
    let Some(result) = common::byte_diff_against_kotlinc(
        "ContinueInDoWhileCondition",
        src,
        "ContinueInDoWhileConditionKt",
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("a do-while condition continue is byte-identical to kotlinc");
}
