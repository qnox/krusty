//! Named arguments to a CLASSPATH (kotlinc-compiled) top-level function: `describe(count = 3,
//! name = "hi")` calls a library function out of declared order. krusty must read the parameter NAMES
//! from the callee's `@Metadata` (`ValueParameter.name`), reorder the arguments into parameter order,
//! and emit a correct `invokestatic` — verified by running the result on a real JVM.
//!
//! This is the general feature that compose-ui DEP cases need (they call androidx functions with
//! named args). The dependency is compiled by the REAL kotlinc, so its `@Metadata` is authoritative.

use std::fs;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn named_args_to_classpath_top_level_fn_reorder_and_run() {
    let Some(_kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_named_args_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    // 1. A library with a plain (non-inline) top-level function, compiled by the real kotlinc so its
    //    `@Metadata` carries the source parameter NAMES.
    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package lib\nfun describe(name: String, count: Int): String = name + \" x\" + count\n",
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

    // 2. A caller using NAMED arguments OUT OF ORDER (`count` before `name`). Correct output requires
    //    krusty to map each label to its parameter position from the callee's @Metadata names.
    let main_src = "import lib.describe\n\
        fun box(): String {\n\
        \x20   val r = describe(count = 3, name = \"hi\")\n\
        \x20   return if (r == \"hi x3\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a classpath named-argument call");

    // 3. The reordered call verifies and returns the right string on a real JVM (the lib dir is on the
    //    runtime classpath — `describe` is a real `invokestatic`, not inlined).
    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn classpath_reordered_named_args_with_trailing_lambda() {
    // A CLASSPATH top-level function with a final function-type parameter (the
    // `NavHost(navController, startDestination, modifier, builder)` shape — composeNav passes every
    // value, so the call has exact arity). The caller passes the leading NAMED arguments OUT OF ORDER
    // and a SYNTACTIC trailing lambda for the last parameter. krusty must (1) reorder the named args
    // from the callee's @Metadata names, (2) bind the trailing lambda to the LAST parameter (not the
    // next free positional slot, which a reordered named arg may already occupy) — then emit a
    // verifying `invokestatic`.
    let Some(_kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_named_tl_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package lib\n\
         fun host(prefix: String, sep: String, block: (StringBuilder) -> Unit): String {\n\
         \x20   val sb = StringBuilder(); sb.append(prefix); sb.append(sep); block(sb); return sb.toString()\n\
         }\n",
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

    // `sep` and `prefix` named OUT OF ORDER (`sep` is param 1, `prefix` is param 0), trailing lambda
    // → `block` (param 2). Result: "p" + "X" + "B" = "pXB".
    let main_src = "import lib.host\n\
        fun box(): String {\n\
        \x20   val r = host(sep = \"X\", prefix = \"p\") { it.append(\"B\") }\n\
        \x20   return if (r == \"pXB\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a classpath named + trailing-lambda call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn named_args_to_classpath_member_fn_reorder_and_run() {
    let Some(_kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_named_args_mem_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    // A library CLASS with an instance method, compiled by kotlinc so its `@Metadata` carries the
    // member's parameter NAMES.
    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package lib\nclass Greeter {\n    fun greet(name: String, count: Int): String = name + \" x\" + count\n}\n",
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

    // A caller invoking the instance member with OUT-OF-ORDER named arguments.
    let main_src = "import lib.Greeter\n\
        fun box(): String {\n\
        \x20   val r = Greeter().greet(count = 5, name = \"hi\")\n\
        \x20   return if (r == \"hi x5\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a classpath member named-argument call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn named_args_to_classpath_extension_fn_reorder_and_run() {
    let Some(_kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_named_args_ext_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    // A library top-level EXTENSION function, compiled by kotlinc so its `@Metadata` carries the source
    // value-parameter names (the receiver is a separate `receiver_type`, NOT a value parameter).
    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package lib\nfun String.tag(name: String, count: Int): String = this + \"/\" + name + \" x\" + count\n",
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

    // Call the extension with OUT-OF-ORDER named arguments (receiver positional, value params labelled).
    let main_src = "import lib.tag\n\
        fun box(): String {\n\
        \x20   val r = \"R\".tag(count = 7, name = \"hi\")\n\
        \x20   return if (r == \"R/hi x7\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a classpath extension named-argument call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}
