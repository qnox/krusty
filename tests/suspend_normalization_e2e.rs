mod common;

#[test]
fn suspending_conditional_can_initialize_a_captured_var() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        suspend fun condition(): Boolean = true
        suspend fun value(): String = "OK"

        fun box(): String {
            var result = "FAIL"
            suspend {
                result = if (condition()) value() else "wrong branch"
            }.startCoroutine(Continuation(EmptyCoroutineContext) {
                it.getOrThrow()
            })
            return result
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendConditionalCaptureWrite"),
        "OK"
    );
}

#[test]
fn checked_unit_coercion_does_not_hide_a_grouped_suspend_call() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        class Deferred<T> {
            suspend fun await(value: T): T = value
        }

        suspend fun grouped() {
            Deferred<Unit>().await(Unit)
        }

        fun box(): String {
            var result = "FAIL"
            suspend {
                grouped()
                result = "OK"
            }.startCoroutine(Continuation(EmptyCoroutineContext) {
                it.getOrThrow()
            })
            return result
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendGroupedUnitTail"),
        "OK"
    );
}

#[test]
fn ordered_binary_operands_can_both_suspend() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
        import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn
        import kotlin.coroutines.resume
        import kotlin.coroutines.startCoroutine

        suspend fun value(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->
            continuation.resume(21)
            COROUTINE_SUSPENDED
        }

        fun box(): String {
            var result = 0
            suspend {
                result = value() + value()
            }.startCoroutine(Continuation(EmptyCoroutineContext) {
                it.getOrThrow()
            })
            return if (result == 42) "OK" else "FAIL: $result"
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendBinaryOperands"),
        "OK"
    );
}

#[test]
fn ordered_function_invocation_arguments_can_both_suspend() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        suspend fun await(value: String): String = value
        suspend fun zip(combine: (String, String) -> String): String =
            combine(await("O"), await("K"))

        fun box(): String {
            var result = "FAIL"
            suspend { result = zip { left, right -> left + right } }
                .startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
            return result
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendInvokeOrderedArguments"),
        "OK"
    );
}

#[test]
fn inline_loop_tail_capture_write_is_normalized_before_state_splitting() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        suspend fun await(value: String): String = value

        fun box(): String {
            var result = ""
            suspend {
                var index = 0
                while (index < 3) {
                    val value = if (index++ == 1) "$" else if (index == 1) "O" else "K"
                    if (value == "$") continue
                    run { result += await(value) }
                }
            }.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
            return result
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendInlineLoopTailCapture"),
        "OK"
    );
}

#[test]
fn normalized_suspend_corpus_shapes_execute() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "controlflow/for_loops_coroutines.kt",
        "coroutines/castWithSuspend.kt",
        "coroutines/varargCallFromSuspend.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must run rather than be rejected by a normalization-shape gate"
        );
    }
}
