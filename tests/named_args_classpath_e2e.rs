//! Named arguments to a CLASSPATH (kotlinc-compiled) top-level function: `describe(count = 3,
//! name = "hi")` calls a library function out of declared order. krusty must read the parameter NAMES
//! from the callee's `@Metadata` (`ValueParameter.name`), map source arguments to parameter slots,
//! preserve source evaluation order, and emit a correct `invokestatic` — verified by running the result
//! on a real JVM.
//!
//! This is the general feature that compose-ui DEP cases need (they call androidx functions with
//! named args). The dependency is compiled by the REAL kotlinc, so its `@Metadata` is authoritative.

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn named_args_to_classpath_top_level_fn_reorder_and_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    // 1. A library with a plain (non-inline) top-level function, compiled by the real kotlinc so its
    //    `@Metadata` carries the source parameter NAMES.
    let Some(libout) = common::compile_lib(
        "named_args",
        "package lib\nfun describe(name: String, count: Int): String = name + \" x\" + count\n",
    ) else {
        return;
    };

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
}

#[test]
fn unknown_named_args_on_classpath_callables_are_rejected_canonically() {
    let Some(libout) = common::compile_lib(
        "unknown_named_args",
        "package lib\n\
         fun top(a: Int): Int = a\n\
         class Holder { fun member(a: Int): Int = a }\n\
         fun String.extension(a: Int): Int = a\n\
         class Outer { class Inner(val a: Int) }\n",
    ) else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let classpath = vec![libout, stdlib];
    let cases = [
        "import lib.top\nfun bad(): Int = top(z = 1)",
        "import lib.Holder\nfun bad(): Int = Holder().member(z = 1)",
        "import lib.extension\nfun bad(): Int = \"\".extension(z = 1)",
        "import lib.Outer\nfun bad(): Outer.Inner = Outer.Inner(z = 1)",
    ];
    for source in cases {
        let diagnostics =
            common::front_end_diagnostics(source, &classpath, common::jdk_modules().as_deref());
        assert_eq!(
            diagnostics,
            ["no parameter with name 'z' found."],
            "unexpected diagnostics for {source:?}"
        );
    }
}

#[test]
fn classpath_top_level_named_args_preserve_source_eval_order() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let Some(libout) = common::compile_lib(
        "named_args_eval_order",
        "package lib\nfun join(a: String, b: String): String = a + \":\" + b\n",
    ) else {
        return;
    };

    let main_src = "import lib.join\n\
        var seq: Int = 0\n\
        fun next(label: String): String { seq = seq + 1; return label + seq }\n\
        fun box(): String {\n\
        \x20   val r = join(b = next(\"b\"), a = next(\"a\"))\n\
        \x20   return if (r == \"a2:b1\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile side-effecting classpath named-argument call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");
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
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let Some(libout) = common::compile_lib(
        "named_tl",
        "package lib\n\
         fun host(prefix: String, sep: String, block: (StringBuilder) -> Unit): String {\n\
         \x20   val sb = StringBuilder(); sb.append(prefix); sb.append(sep); block(sb); return sb.toString()\n\
         }\n",
    ) else {
        return;
    };

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
}

#[test]
fn named_args_to_classpath_member_fn_reorder_and_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    // A library CLASS with an instance method, compiled by kotlinc so its `@Metadata` carries the
    // member's parameter NAMES.
    let Some(libout) = common::compile_lib(
        "named_args_mem",
        "package lib\nclass Greeter {\n    fun greet(name: String, count: Int): String = name + \" x\" + count\n}\n",
    ) else {
        return;
    };

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
}

#[test]
fn named_classpath_member_vararg_reorders_and_passes_array() {
    common::expect_box_ok_against(
        "named_member_vararg",
        "package sample\n\
         class Formatter {\n\
         \x20   fun join(prefix: String, separator: String, vararg parts: String): String =\n\
         \x20       prefix + parts.joinToString(separator)\n\
         \x20   fun <T> genericJoin(prefix: String, vararg parts: T): String =\n\
         \x20       prefix + parts.joinToString(\"\")\n\
         }\n",
        "import sample.Formatter\n\
         fun box(): String {\n\
         \x20   val values = arrayOf(\"A\", \"B\")\n\
         \x20   val result = Formatter().join(\n\
         \x20       separator = \":\",\n\
         \x20       prefix = \"P\",\n\
         \x20       parts = values,\n\
         \x20   )\n\
         \x20   val generic = Formatter().genericJoin(prefix = \"O\", parts = arrayOf(\"K\"))\n\
         \x20   return if (result == \"PA:B\" && generic == \"OK\") \"OK\" else result + generic\n\
         }\n",
    );
}

#[test]
fn named_args_to_classpath_extension_fn_reorder_and_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    // A library top-level EXTENSION function, compiled by kotlinc so its `@Metadata` carries the source
    // value-parameter names (the receiver is a separate `receiver_type`, NOT a value parameter).
    let Some(libout) = common::compile_lib(
        "named_args_ext",
        "package lib\nfun String.tag(name: String, count: Int): String = this + \"/\" + name + \" x\" + count\n",
    ) else {
        return;
    };

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
}

#[test]
fn named_args_to_classpath_extension_fn_omitted_default_uses_slots() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let Some(libout) = common::compile_lib(
        "named_args_ext_default",
        "package lib\n\
         fun String.tag(a: String = \"A\", b: String = \"B\"): String = this + \"/\" + a + \"/\" + b\n",
    ) else {
        return;
    };

    let main_src = "import lib.tag\n\
        fun box(): String {\n\
        \x20   val r = \"R\".tag(b = \"ok\")\n\
        \x20   return if (r == \"R/A/ok\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a named extension default call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");
}

#[test]
fn implicit_receiver_classpath_extension_default_uses_slots() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let Some(libout) = common::compile_lib(
        "named_args_ext_default_implicit",
        "package lib\n\
         fun String.tag(a: String = \"A\", b: String = \"B\"): String = this + \"/\" + a + \"/\" + b\n",
    ) else {
        return;
    };

    let main_src = "import lib.tag\n\
        fun String.wrap(): String = tag(b = \"ok\")\n\
        fun box(): String {\n\
        \x20   val r = \"R\".wrap()\n\
        \x20   return if (r == \"R/A/ok\") \"OK\" else \"fail:\" + r\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile an implicit receiver extension default call");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");
}
