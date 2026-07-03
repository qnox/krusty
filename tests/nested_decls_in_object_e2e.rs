//! An `object` body containing nested type declarations — `class`, `object`, `interface`,
//! `data class`, `enum class`, `annotation class`, and `typealias`. The parser recognises each of
//! these in an object body (a distinct match arm per keyword) and parses it, so declaring one of each
//! exercises those arms. Only a plain member is referenced, so nothing depends on the nested types
//! being lowered.

mod common;

#[test]
fn object_body_with_nested_decls() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping nested_decls_in_object_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping nested_decls_in_object_e2e: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = "object Reg {\n\
        \x20   class C\n\
        \x20   object O\n\
        \x20   interface I\n\
        \x20   data class D(val x: Int)\n\
        \x20   enum class E { A, B }\n\
        \x20   annotation class An\n\
        \x20   typealias T = Int\n\
        \x20   fun value(): Int = 42\n\
        }\n\
        fun box(): String = if (Reg.value() == 42) \"OK\" else \"no\"\n";
    let Some(out) = common::compile_and_run_box(src, "NestedInObj", &[stdlib], Some(&jdk)) else {
        panic!("compile/run returned None");
    };
    assert_eq!(out, "OK");
}
