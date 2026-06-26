//! A dotted (qualified) extension RECEIVER type — `fun A.B.foo()` on a nested class, or `fun
//! Foo.Companion.bar()` — was a parse error ("expected '('"); the parser read only the first segment as
//! the receiver. Now the receiver type may be a dotted path. Verified end-to-end on a real JVM for the
//! nested-class case.
mod common;
#[test]
fn nested_class_dotted_extension_receiver_compiles_and_runs() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::env::var("JAVA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|jh| std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp = std::slice::from_ref(&stdlib);
    let src = "class A { class B(val n: Int) }\n\
        fun A.B.doubled(): Int = n * 2\n\
        fun box(): String = if (A.B(21).doubled() == 42) \"OK\" else \"fail\"\n";
    let classes = common::compile_in_process(src, "T", cp, jdk.as_deref())
        .expect("krusty failed to compile a dotted (nested-class) extension receiver");
    match common::run_box(&classes, "TKt", cp) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
