//! Constructor type-argument inference: `Box("hi")` — with NO explicit `Box<String>` annotation —
//! infers `T = String` from the constructor argument, so the value's type is `Box<String>` and a later
//! `.v` / `.get()` resolves to `String` (members of the result resolve; codegen inserts the checkcast).
//! Mirrors the explicit-annotation case (`val b: Box<String> = Box("hi")`), now without the annotation.
//! Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn inferred_ctor_type_args_resolve_members() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping inferred_ctor_type_args_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inferred_ctor_type_args_e2e: no kotlin-stdlib jar found");
        return;
    };
    // No `Box<…>` annotation: T inferred from each constructor argument.
    let src = "class Box<T>(val v: T) { fun get(): T = v }\n\
class Pair2<A, B>(val a: A, val b: B)\n\
fun box(): String {\n\
val bs = Box(\"hello\")\n\
if (bs.v.length != 5) return \"f1\"\n\
if (bs.get().length != 5) return \"f2\"\n\
val bi = Box(40)\n\
if (bi.v + 2 != 42) return \"f3\"\n\
val p = Pair2(7, \"hi\")\n\
if (p.a + 1 != 8) return \"f4\"\n\
if (p.b.length != 2) return \"f5\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk))
        .expect("krusty must infer constructor type arguments (Box(\"hi\") → Box<String>)");
    assert_eq!(out, "OK");
}
