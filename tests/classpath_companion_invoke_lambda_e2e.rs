//! A compiled-library `Type(args) { … }` factory whose companion declares `operator fun invoke`
//! with a receiver-lambda parameter. The trailing lambda's implicit `this` must bind to the invoke
//! parameter's receiver before the body is typed, independent of the provider or classifier name.
use super::common;

const LIB: &str = "package lib\n\
    class Scope {\n\
    \x20 fun mark(): String = \"m\"\n\
    }\n\
    class Req(val path: String)\n\
    class PlainFactory private constructor(val result: String) {\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: (Req) -> String): PlainFactory =\n\
    \x20     PlainFactory(handler(Req(\"/plain\")))\n\
    \x20 }\n\
    }\n\
    class Factory private constructor(val tag: String) {\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: Scope.(Req) -> String): Factory =\n\
    \x20     Factory(Scope().handler(Req(\"/p\")))\n\
    \x20 }\n\
    }\n\
    class SuspendFactory private constructor() {\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: suspend Scope.(Req) -> String): SuspendFactory =\n\
    \x20     SuspendFactory()\n\
    \x20 }\n\
    }\n\
    typealias AsyncHandler = suspend Scope.(Req) -> String\n\
    interface Provider<T> { fun create(block: (T) -> Unit): Any }\n\
    class AsyncFactory(val handler: AsyncHandler) {\n\
    \x20 constructor(seed: Int, enabled: Boolean) : this({ _ -> if (enabled) seed.toString() else \"\" })\n\
    \x20 companion object : Provider<Int> {\n\
    \x20   override fun create(block: (Int) -> Unit): Any = AsyncFactory { \"created\" }\n\
    \x20   operator fun invoke(handler: AsyncHandler): AsyncFactory = AsyncFactory(handler)\n\
    \x20 }\n\
    }\n\
    suspend fun AsyncFactory.execute(req: Req): String = Scope().handler(req)\n\
    class ContinuationFactory private constructor() {\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: (Req, kotlin.coroutines.Continuation<String>) -> Any): ContinuationFactory =\n\
    \x20     ContinuationFactory()\n\
    \x20 }\n\
    }\n\
    class TwoCtor(val config: Int, val flag: Boolean) {\n\
    \x20 constructor(config: Int) : this(config, false)\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: Scope.(Req) -> String): TwoCtor =\n\
    \x20     TwoCtor(Scope().handler(Req(\"/p\")).length)\n\
    \x20 }\n\
    }\n";

#[test]
fn companion_invoke_receiver_lambda_resolves() {
    // The suspend variant proves suspension does not erase the receiver-function source shape.
    // `TwoCtor` proves public constructors that cannot take the lambda defer their mapping failure
    // to the companion invoke instead of claiming the call prematurely.
    const MAIN: &str = "import lib.Factory\n\
        import lib.SuspendFactory\n\
        import lib.TwoCtor\n\
        fun t() {\n\
        \x20 val f = Factory { req -> mark() + req.path }\n\
        \x20 val s = SuspendFactory { req -> mark() + req.path }\n\
        \x20 val d = TwoCtor { req -> mark() + req.path }\n\
        \x20 f.tag.length + d.config\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("companion_invoke_lambda", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "the trailing lambda must bind the invoke parameter's receiver"
    );
}

#[test]
fn companion_invoke_receiver_lambda_box_runs() {
    const MAIN: &str = "import lib.Factory\n\
        fun box(): String {\n\
        \x20 val f = Factory { req -> mark() + req.path }\n\
        \x20 return if (f.tag == \"m/p\") \"OK\" else \"F:\" + f.tag\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("companion_invoke_lambda_box", LIB, MAIN) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn concrete_function_parameter_shape_crosses_the_compiled_provider_boundary() {
    // The physical descriptor exposes only a raw Function1. The parameter and return classes must
    // come from the callable's semantic signature, not from the classifier name or call origin.
    const MAIN: &str = "import lib.PlainFactory\n\
        fun box(): String {\n\
        \x20 val factory = PlainFactory { request -> request.path }\n\
        \x20 return if (factory.result == \"/plain\") \"OK\" else factory.result\n\
        }\n";
    if let Some(output) = common::expect_box_run_against("semantic_function_parameter", LIB, MAIN) {
        assert_eq!(output, "OK");
    }
}

#[test]
fn suspend_receiver_function_shape_survives_metadata_erasure() {
    // This combines the shapes that make descriptor-only recovery ambiguous: a suspend receiver
    // function behind a typealias, an unrelated companion supertype method, and a same-arity
    // constructor. Metadata's per-parameter SUSPEND_TYPE fact must recover the semantic lambda while
    // the generic companion arbitration—not a provider/name branch—selects the invoke overload.
    const MAIN: &str = "import lib.AsyncFactory\n\
        import lib.Req\n\
        import lib.execute\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String {\n\
        \x20 val factory = AsyncFactory { request -> mark() + request.path }\n\
        \x20 val result = runBlocking { factory.execute(Req(\"/async\")) }\n\
        \x20 return if (result == \"m/async\") \"OK\" else result\n\
        }\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    let Some(library) = common::compile_lib("semantic_suspend_function_parameter", LIB) else {
        return;
    };
    let output = common::expect_box_run(
        MAIN,
        "Main",
        &[library, stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(output, "OK");
}

#[test]
fn explicit_continuation_function_is_not_reclassified_as_suspend() {
    // A source-level continuation parameter has the same generic JVM shape as a suspend function.
    // The absent SUSPEND_TYPE bit is an authoritative discriminator only when the declared Type was
    // actually decoded; keeping both explicit parameters proves the provider did not guess by shape.
    const MAIN: &str = "import lib.ContinuationFactory\n\
        fun accept() {\n\
        \x20 ContinuationFactory { request, continuation -> request.path + continuation.toString() }\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against("semantic_continuation_function_parameter", LIB, MAIN)
    else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "an explicit continuation-taking function must retain both parameters: {diagnostics:?}"
    );
}
