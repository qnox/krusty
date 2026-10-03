use super::common;

/// Corpus `coroutines/kt28844.kt`: a `suspend Unit.() -> Unit` lambda's continuation names its
/// method in `EnclosingMethod`. The `Unit` receiver is a value, so that descriptor cannot spell
/// `V`.
#[test]
fn a_unit_extension_suspend_lambda_loads_its_continuation() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        fun builder(block: suspend Unit.() -> Unit) {
            block.startCoroutine(Unit, Continuation(EmptyCoroutineContext) {})
        }

        var res = "FAIL 1"

        fun testOuter() = builder {
            suspend fun callJobScoped() = "OK"
            val outerJob = suspend {
                res = callJobScoped()
            }
            outerJob()
        }

        fun testLocal() = builder {
            suspend fun callJobScoped() = "OK"
            suspend fun outerJob() {
                res = callJobScoped()
            }
            outerJob()
        }

        fun box(): String {
            testOuter()
            if (res != "OK") return res
            res = "FAIL 2"
            testLocal()
            return res
        }
    "#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "UnitReceiverSuspend"),
        "OK"
    );
}
