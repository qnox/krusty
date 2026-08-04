//! A CLASSPATH top-level EXTENSION function whose value parameter is a RECEIVER function type
//! (`fun Client.cfg(block: Builder.() -> Unit)`) must bind the lambda's implicit `this` to the
//! declared receiver, exactly like a module declaration. Before, the classpath extension path
//! published its call shape without the receiver-lambda marks, so a bare member or extension call
//! inside the block (`member("t")`, `ext("t")`) reported "unresolved function" — a false positive
//! on any ktor-style builder DSL (`http.post(url) { bearerAuth(t) }`). The non-extension form
//! (`fun build(block: Builder.() -> Unit)`) already worked; plain, `inline`, and `suspend`
//! extension variants all failed the same way.
use super::common;

const LIB: &str = "package lib\n\
    class Builder {\n\
    \x20 var token: String = \"\"\n\
    \x20 fun member(v: String) { token = v }\n\
    }\n\
    class Client\n\
    fun Client.cfg(block: Builder.() -> Unit): Builder {\n\
    \x20 val b = Builder()\n\
    \x20 b.block()\n\
    \x20 return b\n\
    }\n\
    inline fun Client.icfg(block: Builder.() -> Unit): Builder {\n\
    \x20 val b = Builder()\n\
    \x20 b.block()\n\
    \x20 return b\n\
    }\n\
    suspend fun Client.post(url: String, block: Builder.() -> Unit): String {\n\
    \x20 val b = Builder()\n\
    \x20 b.block()\n\
    \x20 return url + b.token\n\
    }\n\
    fun Builder.ext(t: String) { token = t }\n";

#[test]
fn classpath_extension_lambda_binds_member_call() {
    const MAIN: &str = "import lib.Client\n\
        import lib.cfg\n\
        fun box(): String {\n\
        \x20 val b = Client().cfg { member(\"OK\") }\n\
        \x20 return b.token\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_ext_lambda_member", LIB, MAIN) {
        assert_eq!(out, "OK", "member call in receiver lambda");
    }
}

#[test]
fn classpath_extension_lambda_binds_extension_call() {
    const MAIN: &str = "import lib.Client\n\
        import lib.cfg\n\
        import lib.ext\n\
        fun box(): String {\n\
        \x20 val b = Client().cfg { ext(\"OK\") }\n\
        \x20 return b.token\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_ext_lambda_ext", LIB, MAIN) {
        assert_eq!(out, "OK", "extension call in receiver lambda");
    }
}

#[test]
fn classpath_inline_extension_lambda_binds_member_call() {
    const MAIN: &str = "import lib.Client\n\
        import lib.icfg\n\
        fun box(): String {\n\
        \x20 val b = Client().icfg { member(\"OK\") }\n\
        \x20 return b.token\n\
        }\n";
    if let Some(out) = common::expect_box_run_against("cp_ext_lambda_inline", LIB, MAIN) {
        assert_eq!(out, "OK", "member call in inline receiver lambda");
    }
}

#[test]
fn source_extension_lambda_binds_member_and_extension_calls() {
    // The SOURCE-origin counterpart: a same-module extension with a receiver-lambda parameter
    // publishes its marks through `CallSig::source`, a separate channel from the classpath
    // provider — both must bind the block's implicit `this`.
    const SRC: &str = "class Builder {\n\
        \x20 var token: String = \"\"\n\
        \x20 fun member(v: String) { token = v }\n\
        }\n\
        class Client\n\
        fun Builder.ext(t: String) { token += t }\n\
        fun Client.cfg(block: Builder.() -> Unit): Builder {\n\
        \x20 val b = Builder()\n\
        \x20 b.block()\n\
        \x20 return b\n\
        }\n\
        fun box(): String = Client().cfg { member(\"O\"); ext(\"K\") }.token\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "source_ext_recv_lambda")
            .expect("strict helper always returns Some"),
        "OK",
        "source extension receiver lambda"
    );
}

#[test]
fn classpath_suspend_extension_lambda_binds_calls() {
    // The ktor `http.post(url) { bearerAuth(t) }` shape: a SUSPEND classpath extension with a
    // receiver-lambda parameter. The checker must accept the block's bare member/extension calls.
    const MAIN: &str = "import lib.Client\n\
        import lib.ext\n\
        import lib.post\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun go(c: Client): String = c.post(\"O\") { ext(\"K\"); member(token) }\n\
        fun box(): String = runBlocking { go(Client()) }\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    let Some(libout) = common::compile_lib("cp_ext_lambda_suspend", LIB) else {
        return;
    };
    let out = common::expect_box_run(
        MAIN,
        "Main",
        &[libout, stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out, "OK", "suspend receiver lambda");
}
