use super::common;

fn numeric_api() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let source = r#"
        package fixtures;
        public final class NumericApi {
            public static String FIELD = "field";
            public static String mixed(int first, long second) { return "mixed"; }
            public static String mixed(long first, long second) { return "wide"; }
            public static String pick(int value) { return "int"; }
            public static String pick(long value) { return "long"; }
            public static String onlyLong(long value) { return Long.toString(value); }
            public String instanceOnlyLong(long value) { return Long.toString(value); }
            public static String cross(int first, long second) { return "left"; }
            public static String cross(long first, int second) { return "right"; }
            public static String incomparable(int first, Object second) { return "first"; }
            public static String incomparable(long first, String second) { return "second"; }
        }
    "#;
    let (classes, _) = common::javac_compile(&[("NumericApi.java".into(), source.into())], &[])?;
    let root = classes.parent()?.to_path_buf();
    Some((classes, root))
}

#[test]
fn static_fields_and_integer_literal_arguments_use_declared_types() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let classpath = vec![java_classes, stdlib];
    let source = r#"
        import fixtures.NumericApi

        val field = NumericApi.FIELD
        val qualifiedField = fixtures.NumericApi.FIELD
        class Holder { val field = NumericApi.FIELD }

        fun box(): String {
            if (NumericApi.mixed(1, 2) != "mixed") return "mixed"
            if (NumericApi.pick(1) != "int") return "pick"
            if (NumericApi.onlyLong(-1) != "-1") return "negative"
            if (fixtures.NumericApi.onlyLong(4) != "4") return "qualified call"
            if (NumericApi().instanceOnlyLong(+2) != "2") return "instance"
            if (field != "field") return "field"
            if (qualifiedField != "field") return "qualified field"
            if (Holder().field != "field") return "member field"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(&jdk))
        .unwrap_or_else(|| {
            panic!(
                "compile static interop: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(&jdk))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run static interop");
    let _ = std::fs::remove_dir_all(temp_root);
    assert_eq!(output.trim(), "OK");
}

#[test]
fn non_literal_int_does_not_match_long_parameter() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let diagnostics = common::front_end_diagnostics(
        "import fixtures.NumericApi\nfun f(value: Int): String = NumericApi.onlyLong(value)\n",
        &[java_classes],
        Some(&jdk),
    );
    let _ = std::fs::remove_dir_all(temp_root);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static")),
        "{diagnostics:?}"
    );
}

#[test]
fn incomparable_literal_overloads_are_ambiguous() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let classpath = [java_classes];
    for call in [
        "NumericApi.cross(1, 1)",
        "NumericApi.incomparable(1, \"x\")",
    ] {
        let source = format!("import fixtures.NumericApi\nfun f(): String = {call}\n");
        let diagnostics = common::front_end_diagnostics(&source, &classpath, Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("unresolved Java static")),
            "{call}: {diagnostics:?}"
        );
    }
    let _ = std::fs::remove_dir_all(temp_root);
}

#[test]
fn top_level_library_calls_use_literal_origin_after_argument_mapping() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(library) = common::compile_lib(
        "integer_literal_top_level",
        r#"
            package fixtures
            fun mixed(first: Int, second: Long): String = "$first:$second"
            fun pick(value: Int): String = "int"
            fun pick(value: Long): String = "long"
            fun onlyLong(value: Long): String = value.toString()
        "#,
    ) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.mixed
        import fixtures.onlyLong
        import fixtures.pick

        fun box(): String {
            if (mixed(second = 2, first = 1) != "1:2") return "mixed"
            if (pick(1) != "int") return "pick"
            if (onlyLong(-3) != "-3") return "long"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(&jdk))
        .unwrap_or_else(|| {
            panic!(
                "compile top-level interop: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(&jdk))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run top-level interop");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}
