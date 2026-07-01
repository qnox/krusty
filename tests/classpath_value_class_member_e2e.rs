//! e2: a classpath interface/class METHOD whose parameter is a value class is JVM-name-MANGLED
//! (`fun get(id: Vid): Cat` → `get-<hash>(String)`). Resolving it by source name `get` must recover the
//! mangled JVM name + the logical `Vid` parameter type from `@Metadata`, and the call must pass the
//! unboxed underlying — exactly kotlinc's `invokeinterface Port.get-<hash>(String)`.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use std::fs;
mod common;

#[test]
fn classpath_value_class_param_member_resolves_mangled() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_vcmember_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    // A classpath library: a value class, an interface with a value-class-param method, and a factory so
    // the box() can obtain a `Port` without implementing the mangled method itself.
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         @JvmInline value class Vid(val v: String)\n\
         class Cat(val name: String)\n\
         interface Port { fun get(id: Vid): Cat }\n\
         private class PortImpl : Port { override fun get(id: Vid): Cat = Cat(\"cat-\" + id.v) }\n\
         fun makePort(): Port = PortImpl()\n",
    )
    .unwrap();
    let kc = vec![
        "-d".into(),
        libout.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib,
        work.join("Lib.kt").to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc) {
        Some((0, _)) => {}
        Some((_, e)) => panic!("kotlinc(lib): {e}"),
        None => return,
    }
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.makePort\n\
        import lib.Vid\n\
        fun box(): String {\n\
        \x20 val p = makePort()\n\
        \x20 val c = p.get(Vid(\"7\"))\n\
        \x20 return if (c.name == \"cat-7\") \"OK\" else \"fail: ${c.name}\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile value-class-param member call");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
