use super::common;

#[test]
fn base_extension_on_user_subtype() {
    const SRC: &str = "open class A\n\
class B : A()\n\
fun A.tag(): String = \"t\"\n\
fun box(): String {\n\
    val b: B = B()\n\
    return if (b.tag() == \"t\") \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("base ext on user subtype"),
        "OK"
    );
}

#[test]
fn base_extension_on_classpath_subtype() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    const LIB: &str = "package q\n\
        sealed class V {\n\
        \x20 abstract val id: String\n\
        \x20 class Ok(override val id: String, val v: String) : V()\n\
        \x20 class Err(override val id: String, val why: String) : V()\n\
        }\n\
        object Make { fun ok(id: String, v: String): V = V.Ok(id, v)\n\
        \x20 fun err(id: String, why: String): V = V.Err(id, why) }\n";
    let Some(libout) = common::compile_lib("ext_subtype", LIB) else {
        return;
    };
    const MAIN: &str = "import q.V\nimport q.Make\n\
        fun V.render(): String = \"[\" + id + \"]\"\n\
        fun box(): String =\n\
        \x20 if (Make.ok(\"a\", \"x\").render() == \"[a]\" && Make.err(\"b\", \"y\").render() == \"[b]\") \"OK\"\n\
        \x20 else \"fail\"\n";
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &[libout, sl], Some(&jdk))
            .expect("base ext on classpath subtype"),
        "OK"
    );
}
