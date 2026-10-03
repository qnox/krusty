use super::common;

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

/// `SuspendRunnable(::bar)` where `bar` suspends. The adapter class captures the reference;
/// the forwarding method's continuation is a different class. Sharing the name makes the
/// adapter `ContinuationImpl`, and `new …(Function1)` throws `NoSuchMethodError`.
#[test]
fn a_suspending_callable_reference_keeps_its_adapter_class() {
    let source = r#"
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.EmptyCoroutineContext
        import kotlin.coroutines.resume
        import kotlin.coroutines.startCoroutine
        import kotlin.coroutines.suspendCoroutine

        fun interface SuspendRunnable {
            suspend fun invoke()
        }

        var result = "initial"
        var resumeCallback: () -> Unit = {}

        suspend fun bar() {
            suspendCoroutine<Unit> { cont ->
                resumeCallback = { cont.resume(Unit) }
            }
            result = "OK"
        }

        fun box(): String {
            val runnable = SuspendRunnable(::bar)
            (runnable::invoke).startCoroutine(Continuation(EmptyCoroutineContext) {})
            if (result != "initial") return "fail: $result"
            resumeCallback()
            return result
        }
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(source, "SuspendSamAdapter"),
        "OK"
    );

    let classes = common::expect_classes_with_stdlib(source, "SuspendSamAdapter");
    let work = common::scratch_dir().expect("scratch");
    let mut adapter = None;
    let mut continuation = None;
    for (name, bytes) in &classes {
        if !name.contains("fir_sam_delegate") {
            continue;
        }
        let path = work.join(format!("{}.class", name.replace('/', "_")));
        std::fs::write(&path, bytes).expect("write class");
        let text = common::javap(&["-p", &path.to_string_lossy()]).expect("javap");
        let header = text
            .lines()
            .find(|line| line.contains("class "))
            .unwrap_or("");
        if header.contains("ContinuationImpl") {
            continuation = Some(name.clone());
        } else {
            adapter = Some(name.clone());
        }
    }
    assert!(
        adapter.is_some(),
        "the callable-reference adapter class is missing: {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    assert!(
        continuation.is_some(),
        "the forwarding method's continuation class is missing: {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    assert_ne!(adapter, continuation);
    let _ = std::fs::remove_dir_all(&work);
}
