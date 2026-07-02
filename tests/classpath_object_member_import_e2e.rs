//! An unqualified call to a MEMBER function of a classpath `object`, imported through
//! `import Obj.member` and called `member(args)` — kotlin-logging's `private val logger = logger {}`
//! idiom. Kotlin dispatches this on the singleton, so it lowers to `getstatic Obj.INSTANCE;
//! invokevirtual Obj.member`. Three facets, each an `unresolved`/inference error before this fix. One,
//! the call resolves (checker + lowerer) against the object member. Two, a TOP-LEVEL `private val x =
//! member {}` infers its type from the member's return (signature phase) so `x.member()` type-checks.
//! Three, a top-level property whose NAME equals the imported member (`val logger = logger {}`) shadows
//! the import in value position, so `logger.member()` reads the property.
//! The library is compiled by kotlinc (the real object ABI) and consumed by krusty on the classpath.
use std::fs;
mod common;

#[test]
fn classpath_object_member_imported_unqualified() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = sl.to_str().unwrap().to_string();
    let work = std::env::temp_dir().join(format!("krusty_objmember_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("lib");
    fs::create_dir_all(&libout).unwrap();
    fs::write(
        work.join("Lib.kt"),
        "package lib\n\
         class KLogger(val tag: String) { fun info(): String = tag }\n\
         object KotlinLogging { fun logger(block: () -> Unit): KLogger = KLogger(\"OK\") }\n",
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
    // The property `logger` shares the imported member's name (the real kotlin-logging idiom): the
    // top-level `val` shadows the import in value position, and its type is inferred from `logger {}`.
    let main = "import lib.KotlinLogging.logger\n\
        private val logger = logger {}\n\
        fun box(): String {\n\
        \x20 if (logger.info() != \"OK\") return \"fail collide: ${logger.info()}\"\n\
        \x20 val local = logger { }\n\
        \x20 if (local.info() != \"OK\") return \"fail local: ${local.info()}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile unqualified object-member import");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
