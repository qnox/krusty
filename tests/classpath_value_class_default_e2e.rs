//! Value-class parameters COMBINED with defaults / copy / suspend on classpath types — each mangles the
//! JVM name AND involves a synthetic overload:
//!   g1  ctor: value-class param + omitted defaulted param  — `Rec(id = id)` (`Rec(id: Vid, n: Int = 0)`)
//!   f2  `.copy()` on a data class with a value-class param  — `c.copy(name = "b")` (mangled `copy-<h>$default`)
//!   f4  suspend interface method with a value-class param    — `p.get(id)` (`get-<h>(String, Continuation)`)
//! Library compiled by kotlinc (the real mangled/synthetic ABI); consumed by krusty.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use std::fs;
mod common;

fn build_lib(work: &std::path::Path, stdlib: &str) -> Option<std::path::PathBuf> {
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         @JvmInline value class Vid(val v: String)\n\
         class Rec(val id: Vid, val n: Int = 0)\n\
         data class Cat(val id: Vid, val name: String)\n\
         interface Port { suspend fun get(id: Vid): String }\n\
         private class PortImpl : Port { override suspend fun get(id: Vid): String = \"cat-\" + id.v }\n\
         fun makePort(): Port = PortImpl()\n",
    )
    .unwrap();
    let kc = vec![
        "-d".into(),
        libout.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib.to_string(),
        work.join("Lib.kt").to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc) {
        Some((0, _)) => Some(libout),
        Some((_, e)) => panic!("kotlinc(lib): {e}"),
        None => None,
    }
}

#[test]
fn value_class_ctor_default_and_data_copy() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_vcdef_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let Some(libout) = build_lib(&work, &stdlib) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // g1: `Rec(id = id)` — value-class param supplied, defaulted `n` omitted (→ synthetic default ctor).
    // f2: `c.copy(name = "b")` — value-class param `id` omitted, filled from receiver (mangled copy$default).
    let main = "import lib.Rec\n\
        import lib.Vid\n\
        import lib.Cat\n\
        fun box(): String {\n\
        \x20 val r = Rec(id = Vid(\"x\"))\n\
        \x20 if (r.id.v != \"x\" || r.n != 0) return \"fail g1: ${r.id.v},${r.n}\"\n\
        \x20 val r2 = Rec(id = Vid(\"y\"), n = 3)\n\
        \x20 if (r2.n != 3) return \"fail g1b: ${r2.n}\"\n\
        \x20 val c = Cat(Vid(\"p\"), \"a\")\n\
        \x20 val c2 = c.copy(name = \"b\")\n\
        \x20 if (c2.id.v != \"p\" || c2.name != \"b\") return \"fail f2: ${c2.id.v},${c2.name}\"\n\
        \x20 val c3 = c.copy(id = Vid(\"q\"))\n\
        \x20 if (c3.id.v != \"q\" || c3.name != \"a\") return \"fail f2b: ${c3.id.v},${c3.name}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile value-class ctor-default / data-copy");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn suspend_value_class_param_member_resolves_and_emits_cps() {
    // f4: a suspend interface method with a value-class parameter (`get-<hash>(String, Continuation)`).
    // Running needs a coroutine driver; assert instead that it COMPILES (resolution was the bug) and emits
    // the mangled CPS call with the unboxed value-class argument — the exact shape kotlinc produces.
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_vcsusp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let Some(libout) = build_lib(&work, &stdlib) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Port\n\
        import lib.Vid\n\
        suspend fun use(p: Port): String = p.get(Vid(\"7\"))\n\
        fun box(): String = \"OK\"\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile suspend value-class-param member call");
    // The `use` facade method must call the mangled CPS member on the interface with the unboxed value.
    let facade = classes
        .iter()
        .find(|(n, _)| n.ends_with("MainKt"))
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        .expect("MainKt facade emitted");
    assert!(
        facade.contains("get-") && facade.contains("Continuation"),
        "expected a mangled CPS call `get-<hash>(…Continuation…)` in the emitted facade"
    );
    assert!(
        facade.contains("constructor-impl"),
        "expected the value-class argument unboxed via constructor-impl"
    );
}
