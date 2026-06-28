//! A SINGLE (non-overloaded) function with a reference-bounded type-parameter param and an inferred
//! (unannotated, expression-body) return. The deeper checker inference must patch the canonical
//! `Signature::ret` for the resolved overload before codegen. The risk is a tparam param:
//! `resolve_ty` erases `T : Number` to its bound, while a key rebuilt from raw AST in codegen
//! (`ty_of`, which erases a bare type parameter to `Object`) would diverge and emit the old
//! `Unit`-defaulted return for a body that returns a `String` (`-Xverify:all` failure). This pins the
//! generic case the same-name-overload test doesn't reach.

mod common;

#[test]
fn generic_param_inferred_return_keeps_override() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping generic_inferred_return_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping generic_inferred_return_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun <T : Number> show(x: T) = x.toString()\n\
fun box(): String {\n\
val s = show(7)\n\
if (s != \"7\") return \"fail: \" + s\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "G", &[stdlib], Some(&jdk))
        .expect("a generic-param fn with an inferred return must keep that return at codegen");
    assert_eq!(out, "OK");
}
