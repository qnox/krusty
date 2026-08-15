//! A member function called through an IMPLICIT receiver while a non-invokable value of the same
//! name is in scope.
//!
//! Shape (builder DSLs hit this constantly): the receiver class declares BOTH `var schema` and
//! `fun schema(builder: …)`. Inside `apply { schema { … } }` the call must bind the member
//! FUNCTION — a property or local that is not function-typed never claims call syntax in Kotlin.
//! krusty's implicit-receiver lambda-member rung was gated off by the mere existence of the
//! same-named value, so the call fell through to "unresolved function" (or, when another file
//! declared a same-named private top-level, to a bogus private-access error).
use super::common;

fn run_box(src: &str) -> String {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::expect_box_run(src, "Main", &[sl], Some(jdk.as_path()))
}

#[test]
fn property_and_fun_share_name_on_implicit_receiver() {
    const SRC: &str = "class SchemaB { var got: String = \"\"\n fun mark(v: String) { got = v } }\n\
        class ParamB {\n\
            var schema: SchemaB? = null\n\
            fun schema(builder: SchemaB.() -> Unit) { schema = SchemaB().apply(builder) }\n\
        }\n\
        fun direct(): String { val p = ParamB(); p.schema { mark(\"x\") }; return p.schema?.got ?: \"none\" }\n\
        fun nested(): String { val p = ParamB().apply { schema { mark(\"y\") } }; return p.schema?.got ?: \"none\" }\n\
        fun box(): String = if (direct() == \"x\" && nested() == \"y\") \"OK\" else \"fail\"\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn enclosing_param_shadows_member_fun_on_receiver_lambda() {
    // The full builder chain: the shadowing value is an enclosing function PARAMETER, and the
    // receiver's own property/function pair collides too.
    const SRC: &str = "class SchemaB {\n\
            private var fmt: String? = null\n\
            fun fmt(value: String) { fmt = value }\n\
            fun render(): String = fmt ?: \"none\"\n\
        }\n\
        class ParamB { var schema: SchemaB? = null\n\
            fun schema(builder: SchemaB.() -> Unit) { schema = SchemaB().apply(builder) } }\n\
        fun build(fmt: String?): String {\n\
            val p = ParamB().apply { schema { fmt?.let { fmt(it) } } }\n\
            return p.schema?.render() ?: \"none\"\n\
        }\n\
        fun box(): String = if (build(\"x\") == \"x\") \"OK\" else \"fail\"\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn function_typed_local_still_wins_call_syntax() {
    // A FUNCTION-TYPED local of the same name keeps claiming the call (invoke convention), so the
    // narrowed gate must not widen member binding over it.
    const SRC: &str =
        "class B { var got: String = \"\"\n fun tag(f: () -> String) { got = f() } }\n\
        fun use(b: B): String {\n\
            val tag: (() -> String) -> Unit = { b.got = it() + \"!\" }\n\
            with(b) { tag { \"v\" } }\n\
            return b.got\n\
        }\n\
        fun box(): String = if (use(B()) == \"v!\") \"OK\" else \"fail\"\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn local_invokable_by_member_extension_still_wins_call_syntax() {
    // Callable shape is semantic, not limited to Ty::Fun. The local String claims `action {}`
    // through Host's member-extension invoke operator, ahead of the nearer receiver's same-named
    // member function.
    const SRC: &str = "class Receiver {\n\
            fun action(block: () -> String): String = \"member:\" + block()\n\
        }\n\
        class Host {\n\
            operator fun String.invoke(block: () -> String): String = \"local:\" + this + block()\n\
            fun use(): String {\n\
                val action = \"x\"\n\
                return with(Receiver()) { action { \"!\" } }\n\
            }\n\
        }\n\
        fun box(): String = if (Host().use() == \"local:x!\") \"OK\" else \"fail\"\n";
    assert_eq!(run_box(SRC), "OK");
}
