//! A literal lambda that breaks out of, or continues, a loop of its caller is legal only because
//! the inline call inlines it: its body has no standalone method, as kotlinc never writes one.
//! The inline function is this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
class Visits {
    var total = 0

    init {
        var i = 0
        while (i < 5) {
            i++
            runIt {
                if (i == 1) continue
                if (i == 4) break
            }
            total += i
        }
    }
}

fun visits(): Int {
    var total = 0
    var i = 0
    while (i < 5) {
        i++
        runIt {
            if (i == 2) continue
            if (i == 4) break
        }
        total += i
    }
    return total
}

fun box(): String = if (Visits().total == 5 && visits() == 4) "OK" else "FAIL"
"#;

#[test]
fn a_lambda_jumping_to_its_callers_loop_is_only_inlined() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
