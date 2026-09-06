mod common;

#[test]
fn constructor_parameter_keeps_class_type_parameter_inside_initializers() {
    let source = r#"
        import kotlin.coroutines.*

        interface Generator<in T> {
            suspend fun yield(value: T)
        }

        class GeneratedIterator<T>(block: suspend Generator<T>.() -> Unit) : Generator<T> {
            private val nextStep: Continuation<Unit> = block.createCoroutine(
                this,
                object : Continuation<Unit> {
                    override val context = EmptyCoroutineContext
                    override fun resumeWith(result: Result<Unit>) {}
                },
            )

            override suspend fun yield(value: T) {}
        }
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}
