use super::common;

/// `val f = block::invoke` is a `KFunction`. The value has to be a `FunctionReferenceImpl`; an
/// `invokedynamic` lambda does not implement that classifier.
#[test]
fn an_unannotated_invoke_reference_is_a_kfunction() {
    let source = r#"
        import kotlin.reflect.KFunction

        fun box(): String {
            val block: () -> String = { "OK" }
            val f = block::invoke
            if (f !is KFunction<*>) return "FAIL: not a KFunction"
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
        import kotlin.reflect.KFunction

        fun f(a: suspend () -> Unit) {
            val f = a::invoke
            if (f !is KFunction<*>) throw AssertionError("not a KFunction")
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
    let carriers = classes
        .iter()
        .filter_map(|(name, bytes)| {
            let class = krusty::jvm::classreader::parse_class(bytes)
                .unwrap_or_else(|error| panic!("{stem}: unreadable `{name}`: {error:?}"));
            class
                .super_class
                .is_some_and(|superclass| {
                    superclass.matches("kotlin/jvm/internal/FunctionReferenceImpl")
                })
                .then(|| name.clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        carriers.len(),
        1,
        "{stem}: reflective value::invoke FunctionReferenceImpl carriers: {carriers:?}"
    );
}
