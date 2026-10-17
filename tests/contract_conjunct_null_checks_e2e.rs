//! A contract's `returns() implies actual` proves every `x != null` of an `&&` condition.
//!
//! `require(result != null && other != null && …)` leaves both values non-null afterwards. An
//! inferred declaration type reads them before its body is checked, and that pass narrowed only a
//! condition that was a single `x != null`, so the lambda result read `String?`.

use super::common;

const SOURCE: &str = r#"
fun <T> runPlain(block: () -> T): T = block()
fun read(value: String?): String? = value

class Logs(private val text: String?) {
    fun both() = runPlain {
        val result = read(text)
        val other = read("tail")
        require(result != null && other != null && result.isNotEmpty())
        result.length + other.length
    }
    fun reversed() = runPlain {
        val result = read(text)
        check(null != result && result.startsWith("s"))
        result.uppercase()
    }
}

val topLevel = runPlain {
    val value = read("top")
    require(value != null && value.length == 3)
    value.reversed()
}

fun box(): String {
    if (Logs("stdout").both() != 10) return "both"
    if (Logs("stdout").reversed() != "STDOUT") return "reversed"
    if (topLevel != "pot") return "top"
    return "OK"
}
"#;

#[test]
fn every_null_check_of_a_contract_condition_narrows_an_inferred_result() {
    common::expect_box_same_as_kotlinc(SOURCE, "ContractConjunctNullChecks");
    let classes = common::classes_against_kotlinc_module(&[("Main.kt", SOURCE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (class, reference) in &classes.reference {
        assert!(
            classes.krusty[class] == *reference,
            "{class} differs from kotlinc"
        );
    }
}
