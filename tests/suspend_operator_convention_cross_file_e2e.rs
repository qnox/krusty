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

/// `compareTo` (via `a < b`) and `contains` (via `x in b`) were recorded in `docs/SPEC.md` as
/// cross-file RESOLUTION gaps, refused by the front end "with or WITHOUT `suspend`". They are not:
/// both resolve and run across the file boundary. The suspending spellings are checked here; the
/// comparison additionally has a cross-file `suspend` guard in `cross_file_inline_call_e2e`.
///
/// Both results are bound to a local first — assigning a suspending if-EXPRESSION straight into a
/// captured `var` hits an unrelated boundary (`SkipReason::Suspend`, pinned by
/// `coroutine_intrinsics_e2e::suspend_in_an_if_expression_into_a_captured_var_skips_without_a_convention`).
#[test]
fn compare_to_and_contains_cross_file_execute() {
    const CMP_LIB: &str = "class Box(var v: Int)\n\
                           suspend operator fun Box.compareTo(o: Box): Int = v - o.v\n";
    let cmp_main = main_driving("val c = Box(3); val less = b < c; r = if (less) 2 else 9");
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", CMP_LIB), ("Main.kt", cmp_main.as_str())],
        "suspend_compare_to_cross_file",
    );

    const IN_LIB: &str = "class Box(var v: Int)\n\
                          suspend operator fun Box.contains(i: Int): Boolean = i == v\n";
    let in_main = main_driving("val has = 1 in b; r = if (has) 2 else 9");
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", IN_LIB), ("Main.kt", in_main.as_str())],
        "suspend_contains_cross_file",
    );
}

/// `invoke` IS still out of reach across the file boundary, so the SPEC entry keeps a residual gap —
/// just a narrower one than it used to claim, and for a different reason. Both halves are pinned:
/// the same declaration reached from its OWN file emits and runs, so this is a cross-file resolution
/// gap and not a property of `invoke` or of `suspend`. Without `suspend` the refusal is SILENT (no
/// diagnostic), which is the part worth noticing if anyone lifts this.
#[test]
fn invoke_convention_cross_file_is_the_residual_gap() {
    const LIB: &str = "class Box(var v: Int)\n\
                       operator fun Box.invoke(): Int = v\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val a = Box(1)\n\
                        \x20   return if (a() == 1) \"OK\" else \"fail\"\n\
                        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path())
        ),
        None,
        "cross-file `invoke` convention: if this now compiles, assert the box() answer instead of \
         deleting the check, and drop the residual gap from docs/SPEC.md"
    );

    const SAME_FILE: &str = "class Box(var v: Int)\n\
                             operator fun Box.invoke(): Int = v\n\
                             fun box(): String {\n\
                             \x20   val a = Box(1)\n\
                             \x20   return if (a() == 1) \"OK\" else \"fail\"\n\
                             }\n";
    assert_eq!(
        common::compile_and_run_box_files(
            &[("Main.kt", SAME_FILE)],
            &[stdlib],
            Some(jdk.as_path())
        ),
        Some("OK".to_string()),
        "the same `invoke` extension must run within its own file — the gap is the file boundary"
    );
}
