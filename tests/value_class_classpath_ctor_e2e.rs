//! Constructing a CLASSPATH `@JvmInline value class` by name (`RoleId("x")`). Such a class exposes only
//! a PRIVATE `<init>` — its public construction surface is the static `constructor-impl(U): U`, which
//! returns the unboxed underlying. krusty reported "unresolved function" (no public ctor) and, once
//! resolved, would have emitted an illegal `new`/`invokespecial` on the private `<init>`. Now: (1) the
//! @Metadata underlying type is recovered from the `box-impl` descriptor when it is carried in the type
//! table (real kotlinc value classes), (2) `resolve_constructor` synthesizes the value-class ctor, and
//! (3) the lowerer emits `constructor-impl` (unboxed) with `x.v` rewritten to identity. Round-tripped on
//! the JVM against a kotlinc-compiled value class (so its @Metadata/ABI is authoritative).

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn classpath_value_class_constructed_by_name() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    // 1. A library with a reference-underlying @JvmInline value class, compiled by the real kotlinc so
    //    its @Metadata carries the value-class marker + underlying type (in the type table).
    let Some(libout) = common::compile_libs(
        "vc_ctor",
        &[(
            "Ids.kt",
            "package ids\n@JvmInline\nvalue class RoleId(val v: String)\n\
         @JvmInline\nvalue class Count(val n: Int)\n",
        )],
    ) else {
        return;
    };

    // 2. A consumer constructing the classpath value class by name and reading its sole property.
    // Constructs both a REFERENCE-underlying (String) and a SCALAR-underlying (Int) classpath value
    // class by name, reads the sole property of each, and uses a derived value — exercising the unboxed
    // representation (identity property access, no illegal private-`<init>` call) for both underlyings.
    let main_src = "import ids.RoleId\nimport ids.Count\n\
        fun box(): String {\n\
        \x20   val r = RoleId(\"ok\")\n\
        \x20   val s = RoleId(\"\" + r.v + r.v)\n\
        \x20   val c = Count(42)\n\
        \x20   val d = Count(c.n + 1)\n\
        \x20   val ok = r.v == \"ok\" && s.v == \"okok\" && c.n == 42 && d.n == 43\n\
        \x20   return if (ok) \"OK\" else \"fail\"\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a classpath value-class construction");

    let Some(out) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");
}
