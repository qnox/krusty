//! A same-file extension declared on a BASE type, called on a value of a SUBTYPE. The lowerer resolves
//! the extension by walking the receiver's supertype closure (user classes AND classpath types) and, for
//! byte parity with kotlinc, upcasts the receiver to the extension's declared type (`checkcast`) before
//! the `invokestatic`. Previously a subtype receiver on a classpath base bailed the IR backend. Runnable.

use super::common;

#[test]
fn base_extension_on_user_subtype() {
    // `fun A.tag()` called on a `B : A` value — resolved through the user supertype chain.
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
    // `fun V.render()` (a same-file extension on a CLASSPATH sealed base) called on a `V.Ok`/`V.Err`
    // value obtained from the library — the receiver's classpath supertype chain is walked to bind it.
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
    // The extension reads only the BASE member `id` (no `is`-narrowing), so this isolates the
    // supertype-walk extension resolution — `render` is declared on `V` but called on `V.Ok`/`V.Err`.
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
