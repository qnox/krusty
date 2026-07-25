use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn bare_java_class_name() {
    const SRC: &str = "package p\n\
        class Widget { fun cls(): String = javaClass.name }\n\
        fun box(): String = Widget().cls()\n";
    assert_eq!(run(SRC), Some("p.Widget".to_string()));
}

#[test]
fn bare_java_class_simple_name() {
    const SRC: &str = "class Widget { fun cls(): String = javaClass.simpleName }\n\
        fun box(): String = Widget().cls()\n";
    assert_eq!(run(SRC), Some("Widget".to_string()));
}

#[test]
fn qualified_java_class_on_value() {
    const SRC: &str = "fun name(x: Any): String = x.javaClass.simpleName\n\
        fun box(): String = name(\"hello\") + \"/\" + name(42)\n";
    assert_eq!(run(SRC), Some("String/Integer".to_string()));
}

#[test]
fn qualified_java_class_plain_class_loader() {
    const SRC: &str = "fun box(): String {\n\
        val cl = \"x\".javaClass.classLoader\n\
        return if (cl != null || \"x\".javaClass.name == \"java.lang.String\") \"OK\" else \"F\"\n\
    }\n";
    assert_eq!(run(SRC), Some("OK".to_string()));
}
