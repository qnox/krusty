//! Repro b4: a `catch` whose parameter sits on the next line(s) inside the parens
//! (`catch (\n e: Exception\n)`) failed to parse ("expected catch parameter name"). The parser now
//! skips newlines around the catch parameter, exactly as an ordinary parameter list allows.
mod common;

#[test]
fn multiline_catch_parameter() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let src = "fun parse(s: String): Int {\n\
        \x20   return try {\n\
        \x20       s.toInt()\n\
        \x20   } catch (\n\
        \x20       e: NumberFormatException\n\
        \x20   ) {\n\
        \x20       -1\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (parse(\"42\") != 42) return \"fail ok\"\n\
        \x20   if (parse(\"x\") != -1) return \"fail catch\"\n\
        \x20   return \"OK\"\n\
        }\n";
    let cp = vec![sl.clone()];
    let classes = common::compile_in_process(src, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a multi-line catch parameter");
    match common::run_box(&classes, "MainKt", &cp) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
