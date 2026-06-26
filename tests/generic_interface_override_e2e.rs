//! A class implementing a generic interface with a concrete type argument (`class StrBox : Container<String>`)
//! overrides the interface method with the substituted type (`fun item(): String`). A call through the
//! interface-typed value (`(c: Container<String>).item()`) resolves to the concrete type — members of the
//! result resolve (`.item().length`) and codegen inserts the checkcast on the erased interface call.
//! Covers a reference type argument (String, checkcast) and a primitive one (Int, unbox). Round-tripped
//! under `-Xverify:all`.

mod common;

#[test]
fn generic_interface_override_resolves() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping generic_interface_override_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping generic_interface_override_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "interface Container<T> { fun item(): T }\n\
class StrBox(val s: String) : Container<String> { override fun item(): String = s }\n\
class IntBox(val n: Int) : Container<Int> { override fun item(): Int = n }\n\
fun box(): String {\n\
val c: Container<String> = StrBox(\"hi\")\n\
if (c.item().length != 2) return \"f1\"\n\
val d: Container<Int> = IntBox(40)\n\
if (d.item() + 2 != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "C", &[stdlib], Some(&jdk))
        .expect("krusty must resolve a generic interface override call (Container<String>.item())");
    assert_eq!(out, "OK");
}
