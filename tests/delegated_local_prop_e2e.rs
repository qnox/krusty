//! Local delegated properties `fun f() { val/var x by Del() }`: a synthesized `$delegate` local holds
//! the delegate; reads route to `getValue(null, propref)`, a `var`'s writes to `setValue`. The
//! delegate's getValue/setValue here ignore the property argument. Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn local_delegated_val_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar found");
        return;
    };
    // Corpus local/localVal.kt + localValNoExplicitType.kt shapes.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = 1\n\
}\n\
fun box(): String {\n\
    val prop: Int by Delegate()\n\
    val inferred by Delegate()\n\
    return if (prop == 1 && inferred == 1) \"OK\" else \"fail\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(SRC, "P", &[stdlib], Some(&jdk))
        .expect("local delegated val should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn local_delegated_var_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar found");
        return;
    };
    // Corpus local/localVar.kt shape.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    var inner = 1\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = inner\n\
    operator fun setValue(t: Any?, p: KProperty<*>, i: Int) { inner = i }\n\
}\n\
fun box(): String {\n\
    var prop: Int by Delegate()\n\
    if (prop != 1) return \"fail get\"\n\
    prop = 2\n\
    if (prop != 2) return \"fail set\"\n\
    return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(SRC, "P", &[stdlib], Some(&jdk))
        .expect("local delegated var should compile + run");
    assert_eq!(out, "OK");
}
