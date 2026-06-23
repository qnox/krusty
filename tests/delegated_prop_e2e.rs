//! Top-level delegated property `val x: T by Del()` where `Del` is a user class with a member
//! `operator fun getValue(thisRef: Any?, property: KProperty<*>): T`. Modeled as `x$delegate` +
//! `x$kprop` (a `PropertyReference0Impl`) statics + a `getX()` calling `delegate.getValue(null, kprop)`.
//! Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn delegated_property_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping delegated_prop_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping delegated_prop_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del {\n\
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = \"hello\"\n\
}\n\
val greeting: String by Del()\n\
fun box(): String {\n\
if (greeting != \"hello\") return \"fail: \" + greeting\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(SRC, "P", &[stdlib], Some(&jdk))
        .expect("delegated property should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn delegated_property_inferred_type_in_clinit() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar found");
        return;
    };
    // Exact shape of corpus accessTopLevelDelegatedPropertyInClinit.kt: inferred type + `val a = prop`.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(thisRef: Any?, prop: KProperty<*>): String {\n\
        return \"OK\"\n\
    }\n\
}\n\
val prop by Delegate()\n\
val a = prop\n\
fun box() = a\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(SRC, "P", &[stdlib], Some(&jdk))
        .expect("inferred-type delegated property should compile + run");
    assert_eq!(out, "OK");
}
