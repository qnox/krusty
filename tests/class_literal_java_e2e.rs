//! Regression coverage for the stdlib `KClass<T>.java` extension. Both a class-literal receiver and
//! a stored `KClass` value resolve through classpath extension-property metadata and round-trip on a
//! real JVM as the corresponding `java.lang.Class`.
use super::common;

#[test]
fn class_literal_dot_java_is_the_java_lang_class() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let cp = vec![sl.clone()];
    let main = "class Widget\n\
        fun box(): String {\n\
        \x20 val c: Class<Widget> = Widget::class.java\n\
        \x20 if (c.simpleName != \"Widget\") return \"fail simple: ${c.simpleName}\"\n\
        \x20 if (c.name != \"Widget\") return \"fail name: ${c.name}\"\n\
        \x20 // `.java` on a non-literal KClass value unwraps at runtime too.\n\
        \x20 val k = Widget::class\n\
        \x20 if (k.java.simpleName != \"Widget\") return \"fail value: ${k.java.simpleName}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile X::class.java");
    match common::run_box(&classes, "MainKt", &[sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
