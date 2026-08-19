use super::common;

fn assert_runs(source: &str) {
    assert_eq!(
        common::checker_diags_with_stdlib(source).expect("checker diagnostics available"),
        Vec::<String>::new()
    );
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "C").expect("compiles and runs"),
        "OK"
    );
}

#[test]
fn expected_type_binds_result_failure_type_param() {
    assert_runs(
        "fun box(): String {\n\
            val result: Result<Int>? = Result.failure(RuntimeException(\"x\"))\n\
            return if (result != null && result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}

#[test]
fn explicit_type_argument_binds_result_failure() {
    assert_runs(
        "fun box(): String {\n\
            val result: Result<Int>? = Result.failure<Int>(RuntimeException(\"x\"))\n\
            return if (result != null && result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}

#[test]
fn non_null_expected_type_binds_result_failure_type_param() {
    assert_runs(
        "fun box(): String {\n\
            val result: Result<Int> = Result.failure(RuntimeException(\"x\"))\n\
            return if (result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}

#[test]
fn named_argument_uses_the_selected_companion_member_shape() {
    assert_runs(
        "fun box(): String {\n\
            val result: Result<Int> = Result.failure(exception = RuntimeException(\"x\"))\n\
            return if (result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}

#[test]
fn imported_companion_member_uses_the_same_callable_shape() {
    assert_runs(
        "import kotlin.Result.Companion.failure\n\n\
        fun box(): String {\n\
            val result: Result<Int> = failure(exception = RuntimeException(\"x\"))\n\
            return if (result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}

#[test]
fn enclosing_parameter_binds_result_failure_type_param() {
    assert_runs(
        "fun consume(result: Result<Int>): String = if (result.isFailure) \"OK\" else \"FAIL\"\n\
        fun box(): String = consume(Result.failure(RuntimeException(\"x\")))\n",
    );
}

#[test]
fn extra_explicit_type_arguments_are_rejected() {
    let diagnostics = common::checker_diags_with_stdlib(
        "fun box(): String {\n\
            val result: Result<Int>? = Result.failure<Int, String>(RuntimeException(\"x\"))\n\
            return \"FAIL\"\n\
        }\n",
    )
    .expect("checker diagnostics available");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics,
        ["one type argument expected for 'fun <T> failure(exception: Throwable): Result<T>'."]
    );
}

#[test]
fn unbound_type_parameter_is_a_cannot_infer_error() {
    let diagnostics = common::checker_diags_with_stdlib(
        "fun box(): String {\n\
            val result = Result.failure(RuntimeException(\"x\"))\n\
            return if (result.isFailure) \"OK\" else \"FAIL\"\n\
        }\n",
    )
    .expect("checker diagnostics available");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics,
        ["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
}

#[test]
fn result_success_argument_inference_binds_the_physical_reference_slot() {
    assert_runs(
        "fun box(): String {\n\
            val result: Result<Int> = Result.success(42)\n\
            return if (result.getOrThrow() == 42) \"OK\" else \"FAIL\"\n\
        }\n",
    );
}
