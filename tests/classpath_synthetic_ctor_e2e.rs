//! Classpath constructor/type resolution that goes through kotlinc's SYNTHETIC constructor overloads
//! and dotted nested names. Each was an `unresolved function`/`unresolved reference` before:
//!   d1  value-class-typed ctor parameter  — `Rec(id = Vid("x"), n = 1)` → `<init>(String, int, marker)`
//!   d2  nested type imported + constructed — `import lib.Scope.Ws; Ws("x")` → `lib/Scope$Ws`
//!   d3  deep FQN nested type reference     — `fun f(x: a.b.Outer.Inner)` → `a/b/Outer$Inner`
//!   d4  ctor omitting a defaulted param    — `Cfg(1)` for `Cfg(a, b = 9)` → `<init>(int, int, mask, marker)`
//! The library is compiled by kotlinc (the real synthetic-ctor ABI) and consumed by krusty on the
//! classpath. Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use std::fs;
mod common;

#[test]
fn classpath_synthetic_ctor_and_nested_type_resolution() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_synctor_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    // A classpath library exercising each synthetic/nested shape.
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         @JvmInline value class Vid(val v: String)\n\
         class Rec(val id: Vid, val n: Int)\n\
         object Scope { class Ws(val s: String) }\n\
         class Cfg(val a: Int, val b: Int = 9, val c: String = \"z\")\n",
    )
    .unwrap();
    fs::write(
        work.join("Ab.kt"),
        "package a.b\nclass Outer { class Inner }\n",
    )
    .unwrap();
    let kc = vec![
        "-d".into(),
        libout.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib,
        work.join("Lib.kt").to_string_lossy().into_owned(),
        work.join("Ab.kt").to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc) {
        Some((0, _)) => {}
        Some((_, e)) => panic!("kotlinc(lib): {e}"),
        None => return,
    }
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Rec\n\
        import lib.Vid\n\
        import lib.Scope.Ws\n\
        import lib.Cfg\n\
        fun deep(x: a.b.Outer.Inner) {}\n\
        fun useWs(x: Ws): String = x.s\n\
        fun makeWs(): Ws = Ws(\"made\")\n\
        fun box(): String {\n\
        \x20 val r = Rec(id = Vid(\"x\"), n = 1)\n\
        \x20 if (r.n != 1) return \"fail d1: ${r.n}\"\n\
        \x20 val rp = Rec(Vid(\"y\"), 2)\n\
        \x20 if (rp.n != 2) return \"fail d1p: ${rp.n}\"\n\
        \x20 val w = Ws(\"nested\")\n\
        \x20 if (w.s != \"nested\") return \"fail d2: ${w.s}\"\n\
        \x20 if (useWs(w) != \"nested\") return \"fail d2-typepos\"\n\
        \x20 val w2: Ws = makeWs()\n\
        \x20 if (w2.s != \"made\") return \"fail d2-ret: ${w2.s}\"\n\
        \x20 val c = Cfg(1)\n\
        \x20 if (c.a != 1 || c.b != 9 || c.c != \"z\") return \"fail d4: ${c.a},${c.b},${c.c}\"\n\
        \x20 val c2 = Cfg(3, 4)\n\
        \x20 if (c2.a != 3 || c2.b != 4) return \"fail d4b: ${c2.a},${c2.b}\"\n\
        \x20 val c3 = Cfg(a = 1, c = \"x\")\n\
        \x20 if (c3.a != 1 || c3.b != 9 || c3.c != \"x\") return \"fail e4: ${c3.a},${c3.b},${c3.c}\"\n\
        \x20 val c4 = Cfg(c = \"q\", a = 5)\n\
        \x20 if (c4.a != 5 || c4.b != 9 || c4.c != \"q\") return \"fail e4b: ${c4.a},${c4.b},${c4.c}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile classpath synthetic-ctor/nested resolution");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
