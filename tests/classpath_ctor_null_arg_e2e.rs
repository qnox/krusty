//! Constructing a CLASSPATH class whose constructor argument list mixes a `null` literal with a
//! descriptor-widened argument (a Kotlin `List` passed where the erased `<init>` takes `java/util/List`)
//! — e.g. `Rec(id, name, desc = null, tags = listOf(…), …)` for a data class with interspersed default
//! parameters. Constructor overload matching reduces both sides to their JVM descriptor identity; a
//! `null` argument was not recognised as fitting a reference parameter there, so the whole list matched
//! no constructor and the call was reported "unresolved function". Round-tripped on the JVM.
use std::fs;
mod common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_ctor_null_arg_with_widened_arg() {
    let Some(_) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let work = std::env::temp_dir().join(format!("krusty_cnull_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         data class Rec(val id: String, val desc: String? = null, val tags: List<String>, val flag: Boolean = false, val n: Int)\n",
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
    let main = "import lib.Rec\n\
        fun box(): String {\n\
        \x20 val r = Rec(id = \"i\", desc = null, tags = listOf(\"t\"), flag = false, n = 5)\n\
        \x20 val s = Rec(\"j\", \"d\", listOf(\"u\"), true, 7)\n\
        \x20 return if (r.desc == null && r.n == 5 && s.desc == \"d\" && s.flag && s.n == 7) \"OK\" else \"FAIL\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath ctor with a null arg");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
