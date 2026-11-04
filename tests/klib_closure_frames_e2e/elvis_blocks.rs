//! PLATFORM: official-producer serialized Elvis expressions and Native dependency execution.

use super::{assert_official_dependency_closures, DependencyConsumer};
use crate::common;

#[test]
fn serialized_runtime_accessor_leaves_preserve_applied_scalar_and_nullable_results() {
    let library = r#"package fixture
fun pickPairValue(pair: Pair<Int, String?>): String {
    if (pair.first != 7) return "wrong first"
    return pair.second ?: "none"
}
"#;
    let consumer = r#"
fun box(): String {
    if (pickPairValue(Pair(7, null)) != "none") return "nullable second"
    return pickPairValue(Pair(7, "OK"))
}
"#;
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[
                ("Library.kt", library),
                ("Consumer.kt", &format!("package fixture\n{consumer}")),
            ],
            "fixture.ConsumerKt",
            &[],
        ),
        "OK",
    );
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedAppliedPairAccessor",
        DependencyConsumer::Native,
    );
}

fn assert_elvis_program(library: &str, consumer: &str, name: &str) {
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[
                ("Library.kt", library),
                ("Consumer.kt", &format!("package fixture\n{consumer}")),
            ],
            "fixture.ConsumerKt",
            &[],
        ),
        "OK",
    );
    assert_official_dependency_closures(library, consumer, name, DependencyConsumer::Native);
}

#[test]
fn serialized_elvis_parameters_preserve_both_branches() {
    assert_elvis_program(
        r#"package fixture
fun choose(value: String?): String = value ?: "none"
"#,
        r#"
fun box(): String {
    if (choose(null) != "none") return "missing fallback"
    return choose("OK")
}
"#,
        "SerializedElvisParameter",
    );
}

#[test]
fn serialized_elvis_evaluates_left_once_and_fallback_lazily() {
    assert_elvis_program(
        r#"package fixture
fun chooseFrom(left: () -> String?, fallback: () -> String): String =
    left() ?: fallback()
"#,
        r#"
fun box(): String {
    var leftCalls = 0
    var fallbackCalls = 0
    val present = chooseFrom({ leftCalls++; "OK" }, { fallbackCalls++; "wrong fallback" })
    if (present != "OK" || leftCalls != 1 || fallbackCalls != 0) return "present branch"
    val absent = chooseFrom({ leftCalls++; null }, { fallbackCalls++; "OK" })
    if (absent != "OK" || leftCalls != 2 || fallbackCalls != 1) return "absent branch"
    return "OK"
}
"#,
        "SerializedElvisEffects",
    );
}

#[test]
fn serialized_nested_elvis_preserves_evaluation_order_and_return_targets() {
    assert_elvis_program(
        r#"package fixture
fun chooseThree(first: () -> String?, second: () -> String?, last: () -> String): String =
    first() ?: second() ?: last()
fun returnOnMissing(value: String?): String {
    val present = value ?: return "none"
    return present
}
"#,
        r#"
fun box(): String {
    var order = ""
    if (chooseThree({ order += "1"; "OK" }, { order += "2"; null }, { order += "3"; "bad" }) != "OK" || order != "1") return "first"
    order = ""
    if (chooseThree({ order += "1"; null }, { order += "2"; "OK" }, { order += "3"; "bad" }) != "OK" || order != "12") return "second"
    order = ""
    if (chooseThree({ order += "1"; null }, { order += "2"; null }, { order += "3"; "OK" }) != "OK" || order != "123") return "last"
    if (returnOnMissing(null) != "none") return "return target"
    return returnOnMissing("OK")
}
"#,
        "SerializedNestedElvis",
    );
}
