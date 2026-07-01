//! Repros c1/c2: a `catch` parameter with a leading annotation (`catch (@Marker e: E)`) and a trailing
//! comma (`catch (e: E,)`). Both previously failed to parse; now accepted, matching Kotlin.
mod common;

#[test]
fn catch_parameter_annotation_and_trailing_comma() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let src = "annotation class Marker\n\
        fun a(s: String): Int = try { s.toInt() } catch (@Marker e: NumberFormatException) { -1 }\n\
        fun b(s: String): Int = try { s.toInt() } catch (e: NumberFormatException,) { -2 }\n\
        fun box(): String {\n\
        \x20 if (a(\"7\") != 7) return \"fail a-ok\"\n\
        \x20 if (a(\"z\") != -1) return \"fail a-catch\"\n\
        \x20 if (b(\"9\") != 9) return \"fail b-ok\"\n\
        \x20 if (b(\"z\") != -2) return \"fail b-catch\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let cp = vec![sl.clone()];
    let classes = common::compile_in_process(src, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile catch with annotation / trailing comma");
    match common::run_box(&classes, "MainKt", &cp) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
