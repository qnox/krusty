//! Two overloads of the same name, each an expression body with an INFERRED (unannotated) return of a
//! different type (`fun f(x: Int) = x + 1` : Int, `fun f(s: String) = s + "!"` : String). The inferred
//! returns are recorded per `(name, parameter types)`, so a call binds the right overload's return.
//! Before the fix the override map was keyed by name alone, so the second overload clobbered the first
//! and `f("hi")` was mis-typed as `Int` ("operator cannot be applied to Int and String").

mod common;

#[test]
fn overloaded_inferred_returns_dont_clobber() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping overloaded_inferred_return_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping overloaded_inferred_return_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun f(x: Int) = x + 1\n\
fun f(s: String) = s + \"!\"\n\
fun box(): String {\n\
if (f(1) != 2) return \"fa\"\n\
if (f(\"hi\") != \"hi!\") return \"fb\"\n\
if (f(\"hi\").length != 3) return \"fc\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "F", &[stdlib], Some(&jdk)).expect(
        "krusty must keep overloaded inferred returns distinct (f(Int):Int, f(String):String)",
    );
    assert_eq!(out, "OK");
}
