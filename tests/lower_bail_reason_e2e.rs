//! Pins for the PRECISE `BackendOutcome::LowerBail` reason each pre-pass-2 lowering gate reports.
//!
//! `src/ir_lower.rs` seeds the caller-owned bail sink with the catch-all `"deep"` and is supposed to
//! refine it as lowering progresses — but every gate/pass-1 bail that returned `None` without a
//! `set_bail` left the reason unrefined, lumping ~195 box-corpus skips into one unactionable survey
//! bucket (`lower: deep`, the top skip-bucket). Each gate now records its precise unsupported-feature
//! boundary so the survey stays attributable; a failure here means a bail path lost (or never got) its
//! label.
//!
use super::common;

#[test]
fn companion_with_explicit_base_arguments_runs() {
    const SOURCE: &str = r#"
open class Base(val x: Int)
class C {
    companion object : Base(1)
}
fun box(): String = if (C.x == 1) "OK" else "FAIL"
"#;
    common::expect_box_ok_with_stdlib(SOURCE, "CompanionBaseArguments");
}

#[test]
fn non_suspend_body_with_same_named_local_is_not_a_suspend_call() {
    // Suspension classification follows the exact checker-selected CALL target. A local variable
    // merely sharing a suspend declaration's name is neither a call nor evidence of continuation
    // threading; retaining the former textual false positive would make unrelated user-chosen names
    // affect backend support and could expose those names through a spurious lowering decision.
    let source = r#"
suspend fun sum(x: Int): Int = x
fun box(): String {
    var sum = 1
    sum += 1
    return if (sum == 2) "OK" else "fail"
}
"#;
    if !common::stdlib_toolchain_ready() {
        return;
    }
    assert_eq!(
        common::inline_source_backend_outcome(source),
        Some(common::BackendOutcome::Emitted),
        "a same-named local value must not be classified as a suspend call"
    );
}

#[test]
fn member_delegate_with_provide_delegate_reports_gate() {
    // A member property whose delegate declares `provideDelegate` isn't modeled by the inline
    // accessor.
    common::assert_inline_source_lower_bail(
        r#"
import kotlin.reflect.KProperty

class Delegate(val v: String) {
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = v
    operator fun provideDelegate(thisRef: Any?, property: KProperty<*>): Delegate = this
}

class C {
    val x: String by Delegate("OK")
}

fun box(): String = C().x
"#,
        "gate:member-delegate-shape",
    );
}
