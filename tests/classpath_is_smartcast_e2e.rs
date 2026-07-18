//! cc1: an `is`-check smart-cast to a CLASSPATH subtype (`val v: V; if (v is V.Ok) v.v`) was not applied.
//! The speculative narrowing type resolver `resolve_ty_no_diag` only resolved same-module (user) classes,
//! type parameters, and a sibling nested type of the enclosing class — a classpath / imported type erased to
//! `Ty::Error`, so the narrowing was dropped and `v` kept its (parent) type, failing with "member … on
//! <parent>". `resolve_ty_no_diag` now also resolves an imported classpath type and a qualified nested one
//! (`imported_type_internal` / `resolve_qualified_nested`, the same resolvers `resolve_ty` uses), so the
//! smart-cast narrows. Runnable end-to-end.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let libout = common::compile_lib(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(&jdk))
}

#[test]
fn is_smartcast_to_qualified_nested_sealed_subclass() {
    // `if (v is V.Ok) v.v` on a value from a classpath call — the reported cc1 shape.
    const LIB: &str = "package q\n\
        sealed class V { class Ok(val v: String) : V()\n class Err : V() }\n\
        object Rules { fun validate(s: String): V = V.Ok(s) }\n";
    const MAIN: &str = "import q.Rules\nimport q.V\n\
        fun box(): String {\n\
        \x20 val v = Rules.validate(\"OK\")\n\
        \x20 return if (v is V.Ok) v.v else \"fail\"\n\
        }\n";
    assert_eq!(
        run("cc1_nested", LIB, MAIN).expect("is V.Ok smart-cast"),
        "OK"
    );
}

#[test]
fn is_smartcast_to_imported_top_level_subclass() {
    // A top-level (non-nested) classpath subclass, imported directly (`is Ok`).
    const LIB: &str = "package q\n\
        sealed class V\n\
        class Ok(val v: String) : V()\n\
        class Err : V()\n\
        object Rules { fun validate(s: String): V = Ok(s) }\n";
    const MAIN: &str = "import q.Rules\nimport q.V\nimport q.Ok\n\
        fun box(): String {\n\
        \x20 val v = Rules.validate(\"OK\")\n\
        \x20 return if (v is Ok) v.v else \"fail\"\n\
        }\n";
    assert_eq!(run("cc1_top", LIB, MAIN).expect("is Ok smart-cast"), "OK");
}

#[test]
fn this_smartcast_reads_classpath_subclass_member() {
    // `when (this) { is V.Ok -> v }` in an extension on a CLASSPATH sealed type: `this` narrows to the
    // subclass, and the implicit member `v` (both the base `id` and the subclass-only `v`) reads through
    // the narrowed type — `checkcast this to V$Ok; invokevirtual`, byte-for-byte kotlinc. Previously the
    // implicit-this narrowed read only consulted same-file classes, so a classpath subclass member was
    // "unresolved reference".
    const LIB: &str = "package q\n\
        sealed class V {\n\
        \x20 abstract val id: String\n\
        \x20 class Ok(override val id: String, val v: String) : V()\n\
        \x20 class Err(override val id: String, val why: String) : V()\n\
        }\n\
        object Make { fun ok(id: String, v: String): V = V.Ok(id, v)\n\
        \x20 fun err(id: String, why: String): V = V.Err(id, why) }\n";
    const MAIN: &str = "import q.V\nimport q.Make\n\
        fun V.render(): String = when (this) {\n\
        \x20 is V.Ok -> id + \":\" + v\n\
        \x20 is V.Err -> id + \"!\" + why\n\
        }\n\
        fun box(): String =\n\
        \x20 if (Make.ok(\"a\", \"x\").render() == \"a:x\" && Make.err(\"b\", \"y\").render() == \"b!y\") \"OK\"\n\
        \x20 else \"fail\"\n";
    assert_eq!(
        run("cc1_this", LIB, MAIN).expect("this smart-cast classpath member"),
        "OK"
    );
}

#[test]
fn is_smartcast_negated_else_branch() {
    // The `!is` / else-branch narrowing (`if (v !is Ok) return "x"; v.v`) also resolves the classpath type.
    const LIB: &str = "package q\n\
        sealed class V { class Ok(val v: String) : V()\n class Err : V() }\n";
    const MAIN: &str = "import q.V\n\
        fun g(v: V): String { if (v !is V.Ok) return \"none\"; return v.v }\n\
        fun box(): String = if (g(V.Ok(\"OK\")) == \"OK\" && g(V.Err()) == \"none\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run("cc1_else", LIB, MAIN).expect("!is else narrowing"),
        "OK"
    );
}
