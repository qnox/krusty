//! Two classpath-resolution gaps against a kotlinc-compiled dependency:
//!   i1  a constructor whose parameter is a Kotlin COLLECTION (`Rule(val v: Set<String>)`):
//!       the JVM `<init>` descriptor erases the param to `Ljava/util/Set;` and drops the `<String>`,
//!       but the call passes `setOf("a")` typed `kotlin/collections/Set<String>` — ctor matching must
//!       bridge the kotlin↔jvm collection identity and erase the type argument.
//!   i2  NAMED arguments on a nested type imported UNQUALIFIED (`import lib.Op.Apply; Apply(a = 1)`):
//!       the positional form already resolves through the nested `$` rewrite; the named form must
//!       resolve the same nested internal before mapping the labels onto the ctor's parameters.
//! Both were `unresolved`/`named arguments … only top-level` before. The library is compiled by kotlinc
//! (the real ABI) and consumed by krusty on the classpath. Needs the JVM toolchain + kotlin-stdlib.
use std::fs;
mod common;

#[test]
fn classpath_collection_param_and_nested_named_ctor() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_coll_nested_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         class Rule(val v: Set<String>)\n\
         class Route(val hops: List<Int>)\n\
         class Pair2(val a: Set<String>, val b: List<Int>)\n\
         class Grid(val cells: Array<Set<String>>)\n\
         object Op { class Apply(val a: Int, val b: Int = 7) }\n",
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
    let main = "import lib.Rule\n\
        import lib.Route\n\
        import lib.Pair2\n\
        import lib.Op.Apply\n\
        fun box(): String {\n\
        \x20 val r = Rule(setOf(\"a\"))\n\
        \x20 if (r.v.size != 1) return \"fail i1: ${r.v.size}\"\n\
        \x20 val rt = Route(listOf(1, 2, 3))\n\
        \x20 if (rt.hops.size != 3) return \"fail i1-list: ${rt.hops.size}\"\n\
        \x20 val p = Pair2(setOf(\"x\"), listOf(9))\n\
        \x20 if (p.a.size != 1 || p.b.size != 1) return \"fail i1-two\"\n\
        \x20 val ap = Apply(a = 1)\n\
        \x20 if (ap.a != 1 || ap.b != 7) return \"fail i2: ${ap.a},${ap.b}\"\n\
        \x20 val ap2 = Apply(a = 2, b = 3)\n\
        \x20 if (ap2.a != 2 || ap2.b != 3) return \"fail i2b: ${ap2.a},${ap2.b}\"\n\
        \x20 val ap3 = Apply(b = 4, a = 5)\n\
        \x20 if (ap3.a != 5 || ap3.b != 4) return \"fail i2c: ${ap3.a},${ap3.b}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile classpath collection-param / nested-named ctor");
    match common::run_box(&classes, "MainKt", &[libout.clone(), sl.clone()]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }

    // A collection nested inside an ARRAY parameter (`Array<Set<String>>` → `[Ljava/util/Set;`) exercises
    // the recursive arm of the descriptor-form normalization: `arrayOf(setOf(...))` types as
    // `Array<kotlin/collections/Set<String>>` and must match the erased `Array<java/util/Set>` parameter.
    // Constructing a reference array (`arrayOf`) end-to-end is an orthogonal, not-yet-lowered feature, so
    // this asserts at the RESOLUTION level (the constructor resolves — no diagnostic), not by running.
    use krusty::diag::DiagSink;
    use krusty::resolve::{check_file, collect_signatures_with_cp};
    let cp_rc = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        libout.clone(),
        sl.clone(),
        jdk.clone(),
    ]));
    // Construction against the array-of-collection parameter (checker-level resolution).
    let features = krusty::features::LangFeatures::from_source("");
    let ctor_main = "import lib.Grid\nfun probe(a: Array<Set<String>>): Grid = Grid(a)\n";
    let mut d2 = DiagSink::new();
    let t2 = krusty::lexer::lex(ctor_main, &mut d2);
    let f2 = vec![krusty::parser::parse_with_features(
        ctor_main, &t2, &mut d2, &features,
    )];
    let platform2 = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp_rc.clone()));
    let mut syms2 = collect_signatures_with_cp(&f2, platform2, &mut d2);
    let _ = check_file(&f2[0], &mut syms2, &mut d2);
    let msgs: Vec<String> = d2.diags.iter().map(|m| m.msg.clone()).collect();
    assert!(
        msgs.is_empty(),
        "Array<Set<String>> ctor param should resolve, got: {msgs:?}"
    );
}
