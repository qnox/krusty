//! Cross-module bytecode inliner (inliner #2): a *branchless* `inline fun` compiled by the real
//! `kotlinc` into a separate library is **spliced** into the caller by krusty — no `invokestatic` to
//! the library function survives, and the result is correct under the JVM verifier. Proves the
//! `Emitter::try_inline_static` → `inline::splice_branchless` path end-to-end.

use std::fs;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn branchless_inline_fn_is_spliced_not_called() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping inline_splice_e2e: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping inline_splice_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inline_splice_e2e: no kotlin-stdlib jar");
        return;
    };
    let stdlib_path = stdlib;
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let _ = (&java_home, &kotlinc);
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_inline_splice_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    // 1. A library with a branchless `inline fun`, compiled by the *real* kotlinc (persistent server).
    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package lib\ninline fun triple(x: Int): Int = x * 3\ninline fun atLeast(x: Int, lo: Int): Int = if (x < lo) lo else x\ninline fun applyIt(x: Int, f: (Int) -> Int): Int = f(x)\n",
    )
    .unwrap();
    let kc_args = vec![
        "-d".to_string(),
        libout.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.clone(),
        lib_kt.to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc_args) {
        Some((0, _)) => {}
        Some((_, e)) => panic!("kotlinc(lib): {e}"),
        None => return,
    }

    // 2. A caller that uses the inline fn, compiled by krusty (in-process) with the lib on its
    // classpath. `a` is a live caller local across the spliced `triple(a)` call — if the splice
    // clobbered its slot, `a + b` would be wrong. Exercises the splice-base (no slot collision).
    let main_src = "import lib.triple\nimport lib.atLeast\nimport lib.applyIt\nfun box(): String {\n    val a = 5\n    val b = triple(a)\n    val c = atLeast(b, 20)\n    val d = atLeast(b, 10)\n    val e = applyIt(b) { it + 1 }\n    return if (a == 5 && b == 15 && c == 20 && d == 15 && e == 16) \"OK\" else \"fail:a=$a b=$b c=$c d=$d e=$e\"\n}\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile");

    // 3. The inline fn was *spliced*, not called: no reference to `triple` survives in MainKt.
    let main_class = &classes
        .iter()
        .find(|(n, _)| n == "MainKt")
        .expect("no MainKt")
        .1;
    for callee in [&b"triple"[..], &b"atLeast"[..], &b"applyIt"[..]] {
        assert!(
            !contains(main_class, callee),
            "MainKt still references `{}` — the inline fn was called, not spliced",
            String::from_utf8_lossy(callee)
        );
    }

    // 4. The spliced bytecode verifies and computes the right result (persistent box JVM). The inline
    // fns were spliced, so MainKt has no runtime dependency on the lib classes.
    let Some(out) = common::run_box(&classes, "MainKt", &[stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

/// Same splice path, but the spliced bodies operate on `Long`/`Float`/`Double` (and an `Int`→`Long`
/// widening). This drives the splice stack-type simulator's category-2 and float/double opcode arms
/// (`lconst`/`fconst`/`dconst`, `ladd`/`dmul`/`fadd`, `i2l`, …) that the all-`Int` case never reaches.
#[test]
fn typed_bodies_are_spliced() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping inline_splice_e2e(typed): set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping inline_splice_e2e(typed): set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping inline_splice_e2e(typed): no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let _ = &kotlinc;
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work =
        std::env::temp_dir().join(format!("krusty_inline_splice_typed_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    let lib_kt = work.join("LibTyped.kt");
    fs::write(
        &lib_kt,
        "package lib\n\
         inline fun dscale(x: Double): Double = x * 2.5\n\
         inline fun lsum(a: Long, b: Long): Long = a + b\n\
         inline fun fbump(x: Float): Float = x + 1.5f\n\
         inline fun widen(x: Int): Long = x.toLong()\n",
    )
    .unwrap();
    let kc_args = vec![
        "-d".to_string(),
        libout.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.clone(),
        lib_kt.to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc_args) {
        Some((0, _)) => {}
        Some((_, e)) => panic!("kotlinc(libTyped): {e}"),
        None => return,
    }

    let main_src = "import lib.dscale\nimport lib.lsum\nimport lib.fbump\nimport lib.widen\n\
        fun box(): String {\n\
        \x20   val d = dscale(4.0)\n\
        \x20   val l = lsum(3L, widen(4))\n\
        \x20   val f = fbump(2.5f)\n\
        \x20   return if (d == 10.0 && l == 7L && f == 4.0f) \"OK\" else \"fail:d=$d l=$l f=$f\"\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "MainTyped", &cp, Some(&jdk_modules))
        .expect("krusty(mainTyped) failed to compile");

    let main_class = &classes
        .iter()
        .find(|(n, _)| n == "MainTypedKt")
        .expect("no MainTypedKt")
        .1;
    for callee in [&b"dscale"[..], &b"lsum"[..], &b"fbump"[..], &b"widen"[..]] {
        assert!(
            !contains(main_class, callee),
            "MainTypedKt still references `{}` — spliced, not called",
            String::from_utf8_lossy(callee)
        );
    }

    let Some(out) = common::run_box(&classes, "MainTypedKt", &[stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "typed box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
