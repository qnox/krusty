//! Member (instance) functions with default parameters, realized via the same `$default` mechanism as
//! data-class `copy`: the JVM backend emits `name$default(self, params…, mask, marker)` and a call with
//! omitted args passes a mask. One node — `MethodCall` with `args[i] = None` for an omitted argument.
//! Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn member_default_args_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping default_args_member_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping default_args_member_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "class C {\n\
fun add(a: Int, b: Int = 10): Int = a + b\n\
fun greet(name: String, greeting: String = \"Hi\"): String = greeting + \" \" + name\n\
}\n\
fun box(): String {\n\
val c = C()\n\
if (c.add(1) != 11) return \"f1\"\n\
if (c.add(1, 2) != 3) return \"f2\"\n\
if (c.greet(\"X\") != \"Hi X\") return \"f3\"\n\
if (c.greet(\"Y\", greeting = \"Yo\") != \"Yo Y\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "D", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn constructor_function_typed_param_lambda_default() {
    // A primary-constructor parameter of function type with a lambda default
    // (`val block: () -> String = { "d" }`) — an omitted argument fills the lambda object at the call
    // site. Covers the no-arg form (default applies) and an explicit-arg form, including a second
    // defaulted parameter after the lambda one. Round-tripped under `-Xverify:all`.
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "class Box(val n: Int = 1, val block: () -> String = { \"d\" })\n\
fun box(): String {\n\
val a = Box()\n\
val b = Box(2, { \"x\" })\n\
if (a.block() != \"d\") return \"f1\"\n\
if (a.n != 1) return \"f2\"\n\
if (b.block() != \"x\") return \"f3\"\n\
if (b.n != 2) return \"f4\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "Box", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
