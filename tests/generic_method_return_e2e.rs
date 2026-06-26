//! Generic METHOD return substitution: a method `fun get(): T` on `class Box<T>` called on a value of
//! a concrete instantiation (`Box<String>`) has result type `String`, not the bare type parameter — so
//! members of the result resolve (`box.get().length`) and codegen inserts the checkcast/unbox kotlinc
//! emits on the erased read. Mirrors the property-read substitution (`Box<Int>().x`), but for a method
//! return. (An inferred receiver type — `Box("hi")` without an explicit `Box<String>` — is a separate,
//! not-yet-supported case: constructor type-argument inference. A `@JvmInline value class` argument is
//! also deliberately not substituted — it needs generic interface bridges krusty does not emit yet.)
//! Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn generic_method_return_substitutes() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping generic_method_return_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping generic_method_return_e2e: no kotlin-stdlib jar found");
        return;
    };
    // `bs.get()` → `String` (checkcast on the erased read); `bi.get()` → `Int` (unbox for `+`).
    let src = "class Box<T>(val v: T) { fun get(): T = v }\n\
fun box(): String {\n\
val bs: Box<String> = Box(\"hello\")\n\
if (bs.get().length != 5) return \"f1\"\n\
val bi: Box<Int> = Box(40)\n\
if (bi.get() + 2 != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk)).expect(
        "krusty must compile a generic method return on a typed receiver (Box<String>.get())",
    );
    assert_eq!(out, "OK");
}
