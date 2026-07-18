use super::common;

#[test]
fn java_static_method_imported_unqualified_resolves() {
    const SRC: &str = "import java.lang.Integer.parseInt\n\
        fun box(): String {\n\
        \x20 val n = parseInt(\"42\") + parseInt(\"8\")\n\
        \x20 return if (n == 50) \"OK\" else \"FAIL:$n\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("static-member import"),
        "OK"
    );
}

#[test]
fn classpath_varargs_static_qualified_and_via_import() {
    const SRC: &str = "import java.util.Arrays\n\
        import java.util.Arrays.asList\n\
        fun box(): String {\n\
        \x20 val a = Arrays.asList(1, 2, 3)\n\
        \x20 val b = asList(\"x\", \"y\")\n\
        \x20 return if (a.size == 3 && a[2] == 3 && b.size == 2 && b[0] == \"x\") \"OK\" else \"FAIL:$a|$b\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("classpath varargs static"),
        "OK"
    );
}

#[test]
fn classpath_object_jvmfield_and_java_static_field_read() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val cs = Charsets.UTF_8\n\
        \x20 return if (cs.name() == \"UTF-8\") \"OK\" else \"FAIL:${cs.name()}\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("static field read"),
        "OK"
    );
}
