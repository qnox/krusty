//! A `Type { lambda }` FACTORY call where `Type` is a CLASSPATH class that HAS constructors and whose
//! companion declares `operator fun invoke(handler)` with a RECEIVER function-type parameter —
//! kotlinc picks the companion `invoke` and shapes the trailing lambda against ITS parameter
//! (binding the lambda's implicit `this` to the declared receiver). Before, the lambda was only
//! shaped against constructor parameters or same-named top-level functions, so the block was typed
//! receiver-less, the companion-invoke fallback could not match it, and the whole call collapsed
//! into "no value passed for parameter …" + "unresolved function" with every bare call inside the
//! block unresolved too. Production hit: ktor's `MockEngine { req -> respond(…) }` (handler is a
//! SUSPEND receiver function type), cascading into `HttpClient(engine) { … }` unresolved because
//! `engine` carried an error type.
//! Needs the JVM toolchain + real kotlinc; skips otherwise.
use super::common;

const LIB: &str = "package lib\n\
    class Scope { fun status(): String = \"S\" }\n\
    class Req(val tag: String)\n\
    class Resp(val body: String)\n\
    typealias Handler = Scope.(Req) -> Resp\n\
    class Config { var handler: Handler? = null }\n\
    class Mock(val config: Config) {\n\
    \x20 constructor(config: Config, oneShot: Boolean) : this(config)\n\
    \x20 companion object {\n\
    \x20   operator fun invoke(handler: Handler): Mock { val c = Config(); c.handler = handler; return Mock(c) }\n\
    \x20 }\n\
    }\n\
    fun Mock.fire(req: Req): Resp = config.handler!!.invoke(Scope(), req)\n\
    fun Scope.respond(content: String): Resp = Resp(content)\n\
    class Client(val engine: Mock, val cfg: Config)\n\
    fun Client(engine: Mock, block: Config.() -> Unit = {}): Client {\n\
    \x20 val c = Config()\n\
    \x20 c.block()\n\
    \x20 return Client(engine, c)\n\
    }\n";

/// The core gap: the trailing lambda must be shaped against the companion `invoke`'s receiver
/// function-type parameter — implicit `this` = `Scope` (member + extension calls bind), the value
/// parameter `req` gets its declared type.
#[test]
fn trailing_lambda_picks_companion_invoke_over_the_constructor() {
    const MAIN: &str = "import lib.Mock\n\
        import lib.Req\n\
        import lib.fire\n\
        import lib.respond\n\
        fun box(): String {\n\
        \x20 val m = Mock { req -> respond(status() + req.tag) }\n\
        \x20 val r = m.fire(Req(\"T\"))\n\
        \x20 return if (r.body == \"ST\") \"OK\" else \"F:\" + r.body\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_companion_invoke_lambda", LIB, MAIN) {
        assert_eq!(out, "OK", "companion invoke trailing lambda");
    }
}

/// kotlinc reports ZERO diagnostics here: the constructor probe's "no value passed for parameter
/// 'config'" must not survive a successful companion-invoke selection.
#[test]
fn companion_invoke_selection_emits_no_constructor_diagnostics() {
    const MAIN: &str = "import lib.Mock\n\
        import lib.respond\n\
        fun mk(): Mock = Mock { req -> respond(req.tag) }\n";
    if let Some(diags) = common::checker_diags_against("cp_companion_invoke_diags", LIB, MAIN) {
        assert_eq!(
            diags,
            Vec::<String>::new(),
            "companion invoke selection must be diagnostic-free"
        );
    }
}

/// A plain (receiver-less) function-type handler: the lambda parameter's type must still be
/// inferred from the `invoke` signature.
#[test]
fn plain_function_handler_infers_the_lambda_parameter() {
    const PLAIN_LIB: &str = "package lib\n\
        class Req(val tag: String)\n\
        class Resp(val body: String)\n\
        class Config { var handler: ((Req) -> Resp)? = null }\n\
        class Mock(val config: Config) {\n\
        \x20 companion object {\n\
        \x20   operator fun invoke(handler: (Req) -> Resp): Mock { val c = Config(); c.handler = handler; return Mock(c) }\n\
        \x20 }\n\
        }\n\
        fun Mock.fire(req: Req): Resp = config.handler!!.invoke(req)\n";
    const MAIN: &str = "import lib.Mock\n\
        import lib.Req\n\
        import lib.Resp\n\
        import lib.fire\n\
        fun box(): String {\n\
        \x20 val m = Mock { req -> Resp(req.tag) }\n\
        \x20 return if (m.fire(Req(\"OK\")).body == \"OK\") \"OK\" else \"fail\"\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_companion_invoke_plain", PLAIN_LIB, MAIN)
    {
        assert_eq!(out, "OK", "plain handler lambda parameter inference");
    }
}

/// The ktor-faithful shape: the handler is a SUSPEND receiver function type behind a typealias, the
/// companion implements an interface, and the class has a same-arity secondary constructor.
#[test]
fn suspend_receiver_handler_lambda_resolves() {
    const SUSPEND_LIB: &str = "package lib\n\
        class Scope { fun status(): String = \"S\" }\n\
        class Req(val tag: String)\n\
        class Resp(val body: String)\n\
        typealias Handler = suspend Scope.(Req) -> Resp\n\
        class Config { var handler: Handler? = null }\n\
        interface Fac<T> { fun create(block: (T) -> Unit): Any }\n\
        class Mock(val config: Config) {\n\
        \x20 constructor(config: Config, oneShot: Boolean) : this(config)\n\
        \x20 companion object : Fac<Config> {\n\
        \x20   override fun create(block: (Config) -> Unit): Any { val c = Config(); block(c); return Mock(c) }\n\
        \x20   operator fun invoke(handler: Handler): Mock { val c = Config(); c.handler = handler; return Mock(c) }\n\
        \x20 }\n\
        }\n\
        suspend fun Mock.fire(req: Req): Resp = config.handler!!.invoke(Scope(), req)\n\
        fun Scope.respond(content: String): Resp = Resp(content)\n";
    const MAIN: &str = "import lib.Mock\n\
        import lib.Req\n\
        import lib.fire\n\
        import lib.respond\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String {\n\
        \x20 val m = Mock { req -> respond(status() + req.tag) }\n\
        \x20 val r = runBlocking { m.fire(Req(\"T\")) }\n\
        \x20 return if (r.body == \"ST\") \"OK\" else \"F:\" + r.body\n\
        }\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    let Some(libout) = common::compile_lib("cp_companion_invoke_suspend", SUSPEND_LIB) else {
        return;
    };
    let out = common::expect_box_run(
        MAIN,
        "Main",
        &[libout, stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out, "OK", "suspend receiver handler lambda");
}

/// The downstream cascade from the acceptance cluster: the factory result feeds a call that names
/// both a class and a same-named top-level function (`HttpClient(engine) { … }`); once the factory
/// resolves, that call must pick the function.
#[test]
fn factory_result_feeds_a_same_named_class_and_function_call() {
    const MAIN: &str = "import lib.Client\n\
        import lib.Mock\n\
        import lib.Req\n\
        import lib.fire\n\
        import lib.respond\n\
        fun box(): String {\n\
        \x20 val m = Mock { req -> respond(req.tag) }\n\
        \x20 val c = Client(m) { }\n\
        \x20 val r = c.engine.fire(Req(\"OK\"))\n\
        \x20 return if (r.body == \"OK\" && c.cfg.handler == null) \"OK\" else \"fail\"\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_companion_invoke_cascade", LIB, MAIN) {
        assert_eq!(out, "OK", "factory result feeding ctor-vs-function call");
    }
}

/// Regression: explicit constructor arguments must still pick the constructor, not the companion
/// `invoke`.
#[test]
fn explicit_constructor_arguments_still_pick_the_constructor() {
    const MAIN: &str = "import lib.Config\n\
        import lib.Mock\n\
        fun box(): String {\n\
        \x20 val m = Mock(Config(), true)\n\
        \x20 return if (m.config.handler == null) \"OK\" else \"fail\"\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_companion_invoke_ctor", LIB, MAIN) {
        assert_eq!(out, "OK", "explicit ctor args pick the ctor");
    }
}

/// The SOURCE-origin counterpart: a same-module class with constructors and a companion
/// `operator fun invoke(handler: Scope.(Req) -> Resp)` — the source channel re-types arguments
/// against the companion signature and must bind the lambda receiver the same way.
#[test]
fn source_companion_invoke_receiver_lambda_resolves() {
    const SRC: &str = "class Scope { fun status(): String = \"S\" }\n\
        class Req(val tag: String)\n\
        class Resp(val body: String)\n\
        class Config { var handler: (Scope.(Req) -> Resp)? = null }\n\
        class Mock(val config: Config) {\n\
        \x20 companion object {\n\
        \x20   operator fun invoke(handler: Scope.(Req) -> Resp): Mock { val c = Config(); c.handler = handler; return Mock(c) }\n\
        \x20 }\n\
        }\n\
        fun Scope.respond(content: String): Resp = Resp(content)\n\
        fun box(): String {\n\
        \x20 val m = Mock { req -> respond(status() + req.tag) }\n\
        \x20 val r = m.config.handler!!.invoke(Scope(), Req(\"T\"))\n\
        \x20 return if (r.body == \"ST\") \"OK\" else \"F:\" + r.body\n\
        }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "source_companion_invoke_lambda"),
        "OK",
        "source companion invoke receiver lambda"
    );
}
