//! Runtime regressions for constructs once rejected by the retired AST-to-IR lowerer.
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
fn member_delegate_with_provide_delegate_runs() {
    // A member property whose delegate declares `provideDelegate` now lowers: the ctor stores the
    // `provideDelegate` result and the accessor calls `getValue` on it (previously
    // `gate:member-delegate-shape`).
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    assert_eq!(
        common::expect_box_run(
            r#"
import kotlin.reflect.KProperty

class Delegate(val v: String) {
    operator fun getValue(thisRef: Any?, property: KProperty<out Any?>): String = v
    operator fun provideDelegate(thisRef: Any?, property: KProperty<out Any?>): Delegate = this
}

class C {
    val x: String by Delegate("OK")
}

fun box(): String = C().x
"#,
            "Main",
            &[sl],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}
