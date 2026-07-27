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
fn aliased_java_static_method_import_resolves() {
    const SRC: &str = "import java.lang.Integer.parseInt as pi\n\
        fun box(): String {\n\
        \x20 val n = pi(\"42\") + pi(\"8\")\n\
        \x20 return if (n == 50) \"OK\" else \"FAIL:$n\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("aliased static import"),
        "OK"
    );
}

#[test]
fn import_alias_is_shadowed_by_a_local_of_the_same_name() {
    const SRC: &str = "import java.lang.Integer.parseInt as v\n\
        fun box(): String {\n\
        \x20 val v = 7\n\
        \x20 return if (v == 7) \"OK\" else \"FAIL:$v\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("alias shadowed by local"),
        "OK"
    );
}

#[test]
fn import_alias_shadowing_is_lexical_not_file_wide() {
    const SRC: &str = "import java.lang.Integer.parseInt as v\n\
        fun parsed(): Int = v(\"42\")\n\
        fun box(): String {\n\
        \x20 val v = 7\n\
        \x20 return if (v == 7 && parsed() == 42) \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("lexically shadowed alias"),
        "OK"
    );
}

#[test]
fn imported_type_alias_resolves_in_type_position() {
    const SRC: &str = "import java.util.ArrayList as ListImpl\n\
        fun size(xs: ListImpl<String>): Int = xs.size\n\
        fun box(): String = if (size(ListImpl<String>()) == 0) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("aliased imported type"),
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
