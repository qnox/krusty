//! e1: `.copy(field = …)` on a CLASSPATH `data class` omitting other fields. kotlinc realizes it through
//! the static `copy$default(receiver, <arg | placeholder>…, int mask, Object marker)` synthetic — the
//! omitted fields come from the receiver. krusty previously mis-lowered it to a garbled `<init>`
//! (VerifyError). Verified byte-identical to kotlinc's `copy$default` call.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
mod common;

#[test]
fn classpath_data_class_copy_with_omitted_fields() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let Some(libout) = common::compile_lib(
        "datacopy",
        "package lib\ndata class Rec(val a: String, val b: String, val n: Int)\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Rec\n\
        fun box(): String {\n\
        \x20 val r = Rec(\"a\", \"b\", 1)\n\
        \x20 val r2 = r.copy(b = \"y\")\n\
        \x20 if (r2.a != \"a\" || r2.b != \"y\" || r2.n != 1) return \"fail e1: ${r2.a},${r2.b},${r2.n}\"\n\
        \x20 val r3 = r.copy(n = 9, a = \"z\")\n\
        \x20 if (r3.a != \"z\" || r3.b != \"b\" || r3.n != 9) return \"fail e1b: ${r3.a},${r3.b},${r3.n}\"\n\
        \x20 val r4 = r.copy()\n\
        \x20 if (r4.a != \"a\" || r4.b != \"b\" || r4.n != 1) return \"fail e1c: ${r4.a},${r4.b},${r4.n}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile classpath data-class copy");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
