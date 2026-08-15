//! Repros c4/c5/c6, each round-tripped on a real JVM against a kotlinc-compiled library:
//!  c4 enum constants and the synthetic `entries` property from a dependency enum
//!  c5 stdlib `Regex(...).matches(s: String)` (a `CharSequence`-param member; `String <: CharSequence`)
//!  c6 a property whose type is a classpath `@JvmInline value class` (`h.id` where `Holder(val id: Vid)`)
use super::common;

#[test]
fn classpath_enum_regex_and_value_class_property() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
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
        \x20 val kinds = Kind.entries\n\
        \x20 if (kinds.size != 2 || kinds[0] != Kind.PENDING || kinds[1] != Kind.DONE) return \"fail c4c\"\n\
        \x20 if (!allLower(\"abc\")) return \"fail c5a\"\n\
        \x20 if (allLower(\"aB9\")) return \"fail c5b\"\n\
        \x20 val h: Holder = makeHolder()\n\
        \x20 if (h.id.v != \"x42\") return \"fail c6: ${h.id.v}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile enum/regex/value-class-property");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn generated_enum_entries_accessor_is_not_a_source_callable_companion_method() {
    let Some(diagnostics) = common::diagnostics_against(
        "enum_entries_not_function",
        "package lib\nenum class Kind { PENDING, DONE }\n",
        "import lib.Kind\nfun invalid() = Kind.getEntries()",
    ) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("getEntries")),
        "the generated accessor must remain hidden from source call resolution: {diagnostics:?}"
    );
}
