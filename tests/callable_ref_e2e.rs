//! Unbound top-level function references `::foo` passed to a function-typed parameter. Lowered to the
//! same `invokedynamic` + `LambdaMetafactory` machinery as a lambda, with the impl method handle
//! pointing directly at the referenced function. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn callable_refs_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping callable_ref_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping callable_ref_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun twice(n: Int): Int = n * 2\n\
fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun box(): String {\n\
if (apply1(::inc, 41) != 42) return \"f1\"\n\
if (apply1(::twice, 21) != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "C", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn bound_member_ref_flows_to_classpath_map() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping callable_ref_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping callable_ref_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "class C(val base: Int) {\n\
fun inc(x: Int) = x + 1\n\
fun add(a: Int, b: Int) = a + b + base\n\
}\n\
fun box(): String {\n\
val c = C(10)\n\
if (c.inc(5) != 6) return \"f1\"\n\
if (c.add(2, 3) != 15) return \"f2\"\n\
val r = listOf(1, 2, 3).map(c::inc)\n\
if (r != listOf(2, 3, 4)) return \"f3:$r\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "BoundMapRef", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn property_ref_keeps_api_and_fits_function_shape() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping callable_ref_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping callable_ref_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "class C(val n: Int)\n\
fun apply1(f: (C) -> Int, c: C): Int = f(c)\n\
fun box(): String {\n\
val p = C::n\n\
if (p.get(C(3)) != 3) return \"get\"\n\
if (p.name != \"n\") return \"name:${p.name}\"\n\
val f: (C) -> Int = p\n\
if (f(C(4)) != 4) return \"fun\"\n\
if (apply1(p, C(5)) != 5) return \"hof\"\n\
if (listOf(C(6)).map(p)[0] != 6) return \"map\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "PropertyRefShape", &[stdlib], Some(&jdk))
    else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn class_literal_type_is_provider_backed() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping callable_ref_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping callable_ref_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "class C\n\
fun box(): String {\n\
val c = C::class\n\
return if (c.name.endsWith(\"C\")) \"OK\" else c.name\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "ClassLiteralShape", &[stdlib], Some(&jdk))
    else {
        return;
    };
    assert_eq!(out, "OK");
}
