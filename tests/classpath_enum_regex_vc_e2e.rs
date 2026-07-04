//! Repros c4/c5/c6, each round-tripped on a real JVM against a kotlinc-compiled library:
//!  c4 enum entry from a classpath enum (`k == Kind.PENDING`)
//!  c5 stdlib `Regex(...).matches(s: String)` (a `CharSequence`-param member; `String <: CharSequence`)
//!  c6 a property whose type is a classpath `@JvmInline value class` (`h.id` where `Holder(val id: Vid)`)
use super::common;

#[test]
fn classpath_enum_regex_and_value_class_property() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let Some(libout) = common::compile_lib(
        "cervc",
        "package lib\n\
         enum class Kind { PENDING, DONE }\n\
         @JvmInline value class Vid(val v: String)\n\
         class Holder(val id: Vid)\n\
         fun makeHolder(): Holder = Holder(Vid(\"x42\"))\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Kind\n\
        import lib.Vid\n\
        import lib.Holder\n\
        import lib.makeHolder\n\
        fun classify(k: Kind): String = if (k == Kind.PENDING) \"p\" else \"d\"\n\
        fun allLower(s: String): Boolean = Regex(\"[a-z]+\").matches(s)\n\
        fun box(): String {\n\
        \x20 if (classify(Kind.PENDING) != \"p\") return \"fail c4a\"\n\
        \x20 if (classify(Kind.DONE) != \"d\") return \"fail c4b\"\n\
        \x20 if (!allLower(\"abc\")) return \"fail c5a\"\n\
        \x20 if (allLower(\"aB9\")) return \"fail c5b\"\n\
        \x20 val h: Holder = makeHolder()\n\
        \x20 if (h.id.v != \"x42\") return \"fail c6: ${h.id.v}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile enum/regex/value-class-property");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
