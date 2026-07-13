//! A CLASSPATH member whose JVM name is MANGLED by a value-class PARAMETER (`findByOrg(id: OrgId):
//! List<Ws>` → `findByOrg-<hash>(String)List`) must still expose its GENERIC return (`List<Ws>`). The
//! mangled-member recovery read only the bare return class (`List`), so `repo.findByOrg(id)` typed as
//! `List<Any>` and a member access on an element (`.name`) failed to resolve. Recover the return from the
//! metadata generic signature. Round-tripped on the JVM.
use std::fs;
mod common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn value_class_param_member_keeps_generic_return() {
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
    let work = std::env::temp_dir().join(format!("krusty_vcpr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         @JvmInline value class OrgId(val value: String)\n\
         data class Ws(val name: String)\n\
         interface Repo { fun findByOrg(id: OrgId): List<Ws> }\n\
         object RepoImpl : Repo { override fun findByOrg(id: OrgId): List<Ws> = listOf(Ws(\"a\"), Ws(\"default\")) }\n",
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
    // `repo.findByOrg(id)` must yield `List<Ws>`, so `firstOrNull { it.name == … }` types `it` as `Ws`.
    let main = "import lib.OrgId\nimport lib.Repo\nimport lib.RepoImpl\n\
        fun box(): String {\n\
        \x20 val repo: Repo = RepoImpl\n\
        \x20 val xs = repo.findByOrg(OrgId(\"o\"))\n\
        \x20 val w = xs.firstOrNull { it.name == \"default\" }\n\
        \x20 return w?.name ?: \"none\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a value-class-param member's generic return");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "default", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
