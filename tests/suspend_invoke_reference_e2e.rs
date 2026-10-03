use super::common;

/// `val f = block::invoke` is a `KFunction`. The value has to be a `FunctionReferenceImpl`; an
/// `invokedynamic` lambda does not implement that classifier.
#[test]
fn an_unannotated_invoke_reference_is_a_kfunction() {
    let source = r#"
        fun box(): String {
            val block: () -> String = { "OK" }
            val f = block::invoke
            return f()
        }
    "#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "InvokeReference"),
        "OK"
    );
    assert_function_reference(source, "InvokeReference");
}

/// Corpus `coroutines/suspendFunctionMethodReference.kt`: `a::invoke` starts as a coroutine.
#[test]
fn an_unannotated_suspend_invoke_reference_is_a_kfunction() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        fun f(a: suspend () -> Unit) {
            val f = a::invoke
            f.startCoroutine(Continuation(EmptyCoroutineContext) {})
        }

        fun box(): String {
            var result = ""
            f { result = "OK" }
            return result
        }
    "#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendInvokeReference"),
        "OK"
    );
    assert_function_reference(source, "SuspendInvokeReference");
}

fn assert_function_reference(source: &str, stem: &str) {
    let classes = common::expect_classes_with_stdlib(source, stem);
    let marker = b"kotlin/jvm/internal/FunctionReferenceImpl";
    assert!(
        classes
            .iter()
            .any(|(_, bytes)| bytes.windows(marker.len()).any(|window| window == marker)),
        "{stem}: reflective value::invoke must be a FunctionReferenceImpl"
    );
}
