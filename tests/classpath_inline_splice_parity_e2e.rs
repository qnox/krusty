//! A classpath `inline` function is spliced into its caller as bytecode. The slots that splice
//! occupies are part of the emitted shape, not an implementation detail: the reference compiler
//! closes the slot of a lambda parameter it inlined away, so every host local below the lambda
//! keeps its number and everything above moves down one.
//!
//! krusty reserved that slot, which shifted every host local up and made the whole method differ
//! from the reference compiler's — before any coroutine is involved. See
//! `docs/JVM_INLINE_BEFORE_CPS.md` §7a.

use super::common;

/// An inline function with BOTH a value parameter and a lambda parameter: the value parameter
/// occupies the slot below the lambda's, so a reserved lambda slot is observable as every later
/// local moving up.
const LIB: &str = r#"
inline fun twice(x: Int, f: (Int) -> Int): Int {
    var acc = 0
    var i = 0
    while (i < 2) {
        val r = f(x + i)
        acc = acc + r
        i = i + 1
    }
    return acc
}

inline fun once(f: () -> Int): Int {
    val r = f()
    return r + 1
}

inline fun pair(f: (Int, Int) -> Int): Int {
    val r = f(7, 8)
    return r + 1
}
"#;

#[test]
fn a_value_parameter_and_a_spliced_lambda_share_the_host_frame() {
    const MAIN: &str = r#"
        fun box(): String {
            val doubled = twice(10) { it * 2 }
            return if (doubled == 42) "OK" else "FAIL: " + doubled
        }
    "#;
    let Some(output) =
        common::expect_box_run_against_ref("inline_splice_value_and_lambda", LIB, MAIN)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(output, "OK");
}

/// The lambda parameter is the ONLY parameter, so its slot is the first the splice would reserve.
#[test]
fn a_lambda_only_inline_call_reserves_no_slot_for_it() {
    const MAIN: &str = r#"
        fun box(): String {
            val v = once { 41 }
            return if (v == 42) "OK" else "FAIL: " + v
        }
    "#;
    let Some(output) = common::expect_box_run_against_ref("inline_splice_lambda_only", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(output, "OK");
}

/// Two spliced lambdas in one body close two slots; the second splice must see the frame the first
/// one left rather than the original numbering.
#[test]
fn two_spliced_lambdas_close_two_slots() {
    const MAIN: &str = r#"
        fun box(): String {
            val a = twice(1) { it + 1 }
            val b = once { a }
            return if (a == 5 && b == 6) "OK" else "FAIL: " + a + "/" + b
        }
    "#;
    let Some(output) = common::expect_box_run_against_ref("inline_splice_two_lambdas", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A multi-parameter lambda stores its arguments into the host frame in reverse (top = last) and
/// then opens with its own inline-depth marker, so three locals of the spliced body land above the
/// host's live ones. Getting that base wrong overlaps a host local that is still live.
#[test]
fn a_multi_parameter_lambda_takes_consecutive_slots_above_the_live_host_frame() {
    const MAIN: &str = r#"
        fun box(): String {
            val v = pair { x, y -> x * y }
            return if (v == 57) "OK" else "FAIL: " + v
        }
    "#;
    let Some(output) = common::expect_box_run_against_ref("inline_splice_two_params", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The lambda reads a capture AND its own parameter: the capture binds to the caller's slot while
/// the parameter belongs to the spliced frame, so the two allocations must not collide.
#[test]
fn a_captured_value_and_a_lambda_parameter_come_from_different_frames() {
    const MAIN: &str = r#"
        fun box(): String {
            val bump = 3
            val v = twice(10) { it + bump }
            return if (v == 27) "OK" else "FAIL: " + v
        }
    "#;
    let Some(output) = common::expect_box_run_against_ref("inline_splice_capture", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(output, "OK");
}
