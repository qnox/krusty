//! A lambda passed to a CLASSPATH MEMBER whose parameter is a Kotlin FUNCTION TYPE
//! (`Regex.replace(input: CharSequence, transform: (MatchResult) -> CharSequence)`) must bind its
//! parameters from that function type, exactly as a lambda passed to a Java SAM parameter or to a
//! stdlib extension already does — including through a RECEIVER function type (`Cfg.() -> R`,
//! which binds `this`), past an omitted defaulted parameter, and through a SAFE call (`re?.replace
//! (s) { … }`). Each shape that goes unrecovered types the lambda's parameters as `Any`, so a
//! member read on them reports "unresolved reference".

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_lambda_param_of_classpath_member_function_type() {
    const SRC: &str = "fun box(): String {\n\
        val re = Regex(\"[ab]\")\n\
        return re.replace(\"aXb\") { m -> m.value.uppercase() } + \"K\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("lambda parameter typed from a member's function-type parameter"),
        "AXBK"
    );
}

#[test]
fn implicit_it_of_classpath_member_function_type() {
    const SRC: &str = "fun box(): String {\n\
        val re = Regex(\"b\")\n\
        return re.replace(\"Ob\") { it.value.uppercase() }\n\
    }\n";
    assert_eq!(
        run(SRC).expect("implicit `it` typed from a member's function-type parameter"),
        "OB"
    );
}

/// A classpath member whose parameter is a RECEIVER function type (`Cfg.() -> String`) or carries a
/// value parameter alongside — the shapes a Java functional interface cannot express, so they are
/// read from the parameter's Kotlin function type rather than from a SAM method.
const LIB: &str = "package lib\n\
    class Cfg(val v: String) {\n\
    \x20 fun build(body: Cfg.() -> String): String = body()\n\
    \x20 fun mapped(f: (String) -> String): String = f(v)\n\
    \x20 fun after(pre: (Int) -> Unit = {}, body: Cfg.() -> String): String { pre(1); return body() }\n\
    }\n\
    object M { fun cfg(): Cfg = Cfg(\"ok\")\n\
    \x20 fun cfgOrNull(): Cfg? = Cfg(\"ok\") }\n";

fn run_with_lib(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let stdlib = common::stdlib_jar()?;
    let lib = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lib, stdlib, jdk.clone()], Some(&jdk))
}

#[test]
fn receiver_lambda_param_of_classpath_member_function_type() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String = M.cfg().build { v.uppercase() }\n";
    assert_eq!(
        run_with_lib("libfun_recv", MAIN).expect("receiver bound from a member's function type"),
        "OK"
    );
}

#[test]
fn value_lambda_param_of_classpath_member_function_type() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String = M.cfg().mapped { s -> s.uppercase() }\n";
    assert_eq!(
        run_with_lib("libfun_value", MAIN).expect("value parameter bound from a function type"),
        "OK"
    );
}

#[test]
fn trailing_lambda_reads_its_own_parameter_past_an_omitted_default() {
    // `after(pre: (Int) -> Unit = {}, body: Cfg.() -> String)` called with only the trailing lambda:
    // the argument's PARAMETER is `body`, not `pre`. Shaping by argument position would type the
    // lambda from `pre` — binding `it: Int` on a `Cfg.()` lambda, which kotlinc rejects and which
    // lowers to a `Cfg` → `Integer` cast at run time.
    const MAIN: &str = "import lib.*\n\
        fun box(): String = M.cfg().after { v.uppercase() }\n";
    assert_eq!(
        run_with_lib("libfun_default", MAIN)
            .expect("trailing lambda shaped from its own parameter"),
        "OK"
    );
}

#[test]
fn wrong_parameter_shape_is_not_borrowed_for_a_trailing_lambda() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(lib) = common::compile_lib("libfun_default_diag", LIB) else {
        return;
    };
    // `it` is not in scope in a `Cfg.() -> String` lambda; borrowing `pre`'s `(Int) -> Unit` shape
    // would silently accept this.
    const MAIN: &str = "import lib.*\n\
        fun f(): String = M.cfg().after { it.toLong(); \"X\" }\n";
    let diagnostics = common::front_end_diagnostics(MAIN, &[lib, stdlib, jdk.clone()], Some(&jdk));
    // Assert on the diagnostic's CONTENT: "not empty" alone would also be satisfied by the call
    // failing to resolve at all, which is the regression the rest of this file exists to catch.
    assert!(
        diagnostics.iter().any(|d| d.contains("'it'")),
        "expected `it` to be unbound against a receiver function type, got: {diagnostics:?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("'after'")),
        "the member itself must still resolve: {diagnostics:?}"
    );
}

/// A SAFE call (`recv?.member { … }`) reaches the same classpath member as `recv.member { … }`, so
/// it must impose the same lambda shape. The safe-call argument seam consulted only source members
/// and extension shapes, never the classpath member expectation, so one `?` away from a working
/// call the lambda's parameters typed as `Any` again.
#[test]
fn safe_call_lambda_param_of_classpath_member_function_type() {
    const SRC: &str = "fun f(re: Regex?, s: String): String? =\n\
        re?.replace(s) { m -> m.value.uppercase() }\n\
        fun box(): String = f(Regex(\"b\"), \"Ob\") ?: \"NULL\"\n";
    assert_eq!(
        run(SRC).expect("safe-call lambda typed from a member's function-type parameter"),
        "OB"
    );
}

#[test]
fn safe_call_receiver_lambda_param_of_classpath_member_function_type() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String = M.cfgOrNull()?.build { v.uppercase() } ?: \"NULL\"\n";
    assert_eq!(
        run_with_lib("libfun_safe_recv", MAIN)
            .expect("safe-call receiver bound from a member's function type"),
        "OK"
    );
}

/// The argument -> parameter mapping is what makes a trailing lambda read `body` rather than the
/// omitted defaulted `pre`; a safe call reaches it through a different seam, so pin it there too.
#[test]
fn safe_call_trailing_lambda_reads_its_own_parameter_past_an_omitted_default() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String = M.cfgOrNull()?.after { v.uppercase() } ?: \"NULL\"\n";
    assert_eq!(
        run_with_lib("libfun_safe_default", MAIN)
            .expect("safe-call trailing lambda shaped from its own parameter"),
        "OK"
    );
}

/// The other carrier of a lambda-implementable parameter: a Java FUNCTIONAL INTERFACE, whose shape
/// comes from its SAM method rather than from a Kotlin function type. It reaches the same seam.
#[test]
fn safe_call_lambda_param_of_classpath_java_sam_member() {
    const JAVA: &str = "package jlib;\n\
        public final class Box {\n\
        \x20 public interface Mapper { String map(String s); }\n\
        \x20 private final String v;\n\
        \x20 private Box(String v) { this.v = v; }\n\
        \x20 public static Box of(String v) { return new Box(v); }\n\
        \x20 public String mapped(Mapper m) { return m.map(v); }\n\
        }\n";
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some((java_classes, _)) =
        common::javac_compile(&[("jlib/Box.java".into(), JAVA.into())], &[])
    else {
        return;
    };
    const MAIN: &str = "import jlib.Box\n\
        fun box(): String = Box.of(\"ok\")?.mapped { s -> s.uppercase() } ?: \"NULL\"\n";
    assert_eq!(
        common::compile_and_run_box(
            MAIN,
            "Main",
            &[java_classes, stdlib, jdk.clone()],
            Some(&jdk)
        )
        .expect("safe-call lambda typed from a Java SAM parameter"),
        "OK"
    );
}

/// A NAMED argument through `?.`: the safe-call arm gates a labelled call separately, and the
/// expectations are mapped through the callable's parameter slots rather than by position.
#[test]
fn safe_call_named_lambda_arguments_resolve_in_frontend() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(lib) = common::compile_lib("libfun_safe_named", LIB) else {
        return;
    };
    const MAIN: &str = "import lib.*\n\
        fun f(c: Cfg?): String? = c?.after(body = { v.uppercase() }, pre = { n -> n.toLong() })\n";
    let diagnostics = common::front_end_diagnostics(MAIN, &[lib, stdlib, jdk.clone()], Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn safe_call_lambda_param_members_resolve_in_frontend() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    const SRC: &str = "fun f(re: Regex?, s: String): String? =\n\
        re?.replace(s) { m -> m.value }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn lambda_param_members_resolve_in_frontend() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    const SRC: &str = "fun f(re: Regex, s: String): String =\n\
        re.replace(s) { m -> m.groupValues[0] }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}
