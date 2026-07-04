//! Repro: classpath qualified-name resolution for a Kotlin `object` singleton, a `logger {}` object
//! member taking a trailing lambda, a sealed-subclass constructor, and an `is Sealed.Sub` check.
//! These were resolved as an unresolved "Java static" because the qualifier is a classpath Kotlin
//! object / nested / sealed type rather than a companion.
use super::common;

#[test]
fn classpath_object_and_nested_resolution() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    // A classpath library: a plain `object`, an object with a trailing-lambda member, and a sealed
    // hierarchy with nested subclasses.
    let Some(libout) = common::compile_lib(
        "cobj",
        "package lib\n\
         object Ids { fun generate(): String = \"id-42\" }\n\
         object L { fun logger(build: () -> String): String = build() }\n\
         sealed class Subject {\n\
         \x20 class User(val name: String) : Subject()\n\
         \x20 object Anon : Subject()\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Ids\n\
        import lib.L\n\
        import lib.Subject\n\
        fun box(): String {\n\
        \x20 if (Ids.generate() != \"id-42\") return \"fail r5a: ${Ids.generate()}\"\n\
        \x20 if (L.logger { \"hi\" } != \"hi\") return \"fail r5b\"\n\
        \x20 val s: Subject = Subject.User(\"x\")\n\
        \x20 if (!(s is Subject.User)) return \"fail r5e\"\n\
        \x20 val u = s as Subject.User\n\
        \x20 if (u.name != \"x\") return \"fail as: ${u.name}\"\n\
        \x20 val a: Subject = Subject.Anon\n\
        \x20 val tag = when (a) {\n\
        \x20   is Subject.User -> \"user\"\n\
        \x20   Subject.Anon -> \"anon\"\n\
        \x20   else -> \"?\"\n\
        \x20 }\n\
        \x20 if (tag != \"anon\") return \"fail when: $tag\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile classpath object/nested resolution");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
