//! A companion initializer runs in the outer class's `<clinit>`, where each read of the companion
//! instance becomes a load of the outer `Companion` field. An inline lambda in that initializer
//! numbers its own values: its value 0 is the lambda's first capture, not the companion, so it is
//! still read from the lambda's own slot. The inline function is this repository's own, compiled
//! by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
class C {
    companion object {
        var visited = ""

        init {
            val first = visited.length + 2
            runIt {
                visited += first
            }
        }
    }
}

fun box(): String = if (C.visited == "2") "OK" else "FAIL ${C.visited}"
"#;

#[test]
fn a_lambda_capture_in_a_companion_initializer_reads_its_own_slot() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
