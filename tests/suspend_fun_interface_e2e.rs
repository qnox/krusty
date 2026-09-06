mod common;

#[test]
fn suspend_function_supertype_is_not_resolved_as_a_classifier_spelling() {
    let source = r#"
        fun interface Foo<P> : suspend (P) -> Unit

        class Bar<P>(foo: Foo<P>)

        fun <P> create(foo: Foo<P>): Bar<P> = Bar(foo)

        class FooImpl<T> : Foo<T> {
            override suspend fun invoke(p1: T) {}
        }

        fun <P> create2(foo: FooImpl<P>): Bar<P> = Bar(foo)

        fun box(): String {
            create<Int> {}
            create2<Int>(FooImpl())
            return "OK"
        }
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn plain_function_value_converts_to_a_suspend_fun_interface() {
    let source = r#"
        fun interface SuspendFun {
            suspend fun method(): String
        }

        fun adapt(implementation: () -> String): SuspendFun = SuspendFun(implementation)
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn direct_lambda_implements_the_suspend_sam_jvm_slot() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.startCoroutine

        fun interface SuspendFun {
            suspend fun method(): String
        }

        fun runBlocking(block: suspend () -> String): String {
            var result = ""
            block.startCoroutine(Continuation(EmptyCoroutineContext) {
                result = it.getOrThrow()
            })
            return result
        }

        fun box(): String {
            val implementation = SuspendFun { "OK" }
            return runBlocking { implementation.method() }
        }
    "#;

    assert_eq!(
        common::compile_and_run_with_stdlib(source, "SuspendSamLiteral")
            .expect("direct lambda -> suspend SAM compile+run"),
        "OK"
    );
}
