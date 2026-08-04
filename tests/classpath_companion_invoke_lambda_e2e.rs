//! A classpath `Type(args) { … }` FACTORY whose companion declares `operator fun invoke` with a
//! RECEIVER-lambda parameter — the ktor `MockEngine { req -> respond(…) }` shape. The trailing
//! lambda's implicit `this` must bind to the invoke parameter's receiver BEFORE the body is
//! typed; without the shape every member/extension inside the block reported "unresolved
//! reference", which then cascaded into "unresolved function 'MockEngine'".
use super::common;

const LIB: &str = "package lib\n\
    class Scope {\n\
    \x20 fun mark(): String = \"m\"\n\
    }\n\
    class Req(val path: String)\n\
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
    class TwoCtor(val config: Int, val flag: Boolean) {\n\
    \x20 constructor(config: Int) : this(config, false)\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: Scope.(Req) -> String): TwoCtor =\n\
    \x20     TwoCtor(Scope().handler(Req(\"/p\")).length)\n\
    \x20 }\n\
    }\n";

#[test]
fn companion_invoke_receiver_lambda_resolves() {
    // The suspend variant is the exact MockEngine parameter shape
    // (`suspend MockRequestHandleScope.(HttpRequestData) -> HttpResponseData`).
    // `TwoCtor` is the exact MockEngine CLASS shape: PUBLIC constructors that cannot take the
    // lambda — their mapping failure must fall through to the companion invoke, not claim the
    // call with "no value passed for parameter".
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
