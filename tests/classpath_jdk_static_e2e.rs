use super::common;

fn numeric_api() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let source = r#"
        package fixtures;
        public class NumericApi {
            public static String FIELD = "field";
            private final String construction;
            public NumericApi() { construction = "default"; }
            public NumericApi(String value) { construction = "fixed"; }
            public NumericApi(String... values) { construction = "vararg"; }
            public String construction() { return construction; }
            public String instanceJoin(String... values) { return String.join("", values); }
            public String prefixedJoin(String prefix, String... values) {
                return prefix + String.join("", values);
            }
            public String nullableJoin(String... values) {
                return (values[0] == null ? "O" : values[0]) + values[1];
            }
            public static String staticJoin(String... values) { return String.join("", values); }
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
            public static int supply(java.util.function.IntSupplier value) {
                return value.getAsInt();
            }
            public static int transform(
                int value,
                java.util.function.IntUnaryOperator operation
            ) {
                return operation.applyAsInt(value);
            }
            public int transformInstance(
                int value,
                java.util.function.IntUnaryOperator operation
            ) {
                return operation.applyAsInt(value);
            }
            public static int adapt(
                int value,
                java.util.function.IntUnaryOperator operation
            ) {
                return operation.applyAsInt(value);
            }
            public static String adapt(
                String value,
                java.util.function.UnaryOperator<String> operation
            ) {
                return operation.apply(value);
            }
            public static int consume(java.util.function.Consumer<String> consumer) {
                consumer.accept("ok");
                return 2;
            }
            public static int consumeBoth(
                java.util.function.BiConsumer<String, String> consumer
            ) {
                consumer.accept("o", "k");
                return 2;
            }
            public static String supplyText(java.util.function.Supplier<String> supplier) {
                return supplier.get();
            }
            public static <T> String combine(T first, T second) {
                return "generic";
            }
            public static <T> String consumePair(
                T first,
                T second,
                java.util.function.Consumer<T> consumer
            ) {
                consumer.accept(second);
                return "generic";
            }
            public <T> String consumePairInstance(
                T first,
                T second,
                java.util.function.Consumer<T> consumer
            ) {
                consumer.accept(second);
                return "generic";
            }
            public int choose(java.util.function.IntUnaryOperator operation) {
                return operation.applyAsInt(1);
            }
            public int choose(java.util.function.Function<String, Integer> operation) {
                return operation.apply("x");
            }
        }
    "#;
    let (classes, _) = common::javac_compile(&[("NumericApi.java".into(), source.into())], &[])?;
    let root = classes.parent()?.to_path_buf();
    Some((classes, root))
}

#[test]
fn static_fields_and_integer_literal_arguments_use_declared_types() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let classpath = vec![java_classes, stdlib];
    let source = r#"
        import fixtures.NumericApi

        val field = NumericApi.FIELD
        val qualifiedField = fixtures.NumericApi.FIELD
        class Holder { val field = NumericApi.FIELD }
        class Child : NumericApi() {
            fun joined(): String = instanceJoin("O", "K")
        }

        fun box(): String {
            if (NumericApi.mixed(1, 2) != "mixed") return "mixed"
            if (NumericApi.pick(1) != "int") return "pick"
            if (NumericApi.onlyLong(-1) != "-1") return "negative"
            if (NumericApi.supply { 7 } != 7) return "sam"
            if (NumericApi.transform(6) { it + 1 } != 7) return "sam parameter"
            if (fixtures.NumericApi.transform(8) { it - 1 } != 7) return "qualified sam parameter"
            if (NumericApi().transformInstance(6) { it + 1 } != 7) return "instance sam parameter"
            if (NumericApi.adapt(6) { it + 1 } != 7) return "overloaded sam"
            if (fixtures.NumericApi.adapt(8) { it - 1 } != 7) return "qualified overloaded sam"
            if (NumericApi.consume { it.length } != 2) return "generic sam input"
            if (NumericApi.consumeBoth { left, right -> left.length + right.length } != 2) return "two-input sam"
            if (NumericApi.supplyText { "ok" } != "ok") return "generic sam return"
            if (NumericApi.combine("x", Any()) != "generic") return "generic non-sam"
            if (NumericApi().consumePairInstance("x", "y") { it.length } != "generic") return "generic instance sam"
            if (NumericApi().construction() != "default") return "default constructor"
            if (NumericApi("x").construction() != "fixed") return "fixed constructor"
            if (NumericApi("x", "y").construction() != "vararg") return "vararg constructor"
            if (Child().joined() != "OK") return "implicit receiver vararg"
            if (NumericApi.staticJoin("O", "K") != "OK") return "static vararg"
            if (NumericApi().nullableJoin(null, "K") != "OK") return "nullable vararg"
            if (fixtures.NumericApi.onlyLong(4) != "4") return "qualified call"
            if (NumericApi().instanceOnlyLong(+2) != "2") return "instance"
            if (field != "field") return "field"
            if (qualifiedField != "field") return "qualified field"
            if (Holder().field != "field") return "member field"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "compile static interop: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run static interop");
    let _ = std::fs::remove_dir_all(temp_root);
    assert_eq!(output.trim(), "OK");
}

#[test]
fn non_literal_int_does_not_match_long_parameter() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    for expression in ["value", "1 / 0", "2_000_000_000 + 2_000_000_000"] {
        let source = format!(
            "import fixtures.NumericApi\n\
             fun f(value: Int): String = NumericApi.onlyLong({expression})\n"
        );
        let diagnostics = common::front_end_diagnostics(
            &source,
            std::slice::from_ref(&java_classes),
            Some(jdk.as_path()),
        );
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("unresolved Java static")),
            "{expression}: {diagnostics:?}"
        );
    }
    let _ = std::fs::remove_dir_all(temp_root);
}

#[test]
fn java_vararg_does_not_omit_fixed_parameters() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let diagnostics = common::front_end_diagnostics(
        "import fixtures.NumericApi\nfun value(): String = NumericApi().prefixedJoin()\n",
        &[java_classes],
        Some(jdk.as_path()),
    );
    let _ = std::fs::remove_dir_all(temp_root);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("none of the following candidates is applicable")),
        "{diagnostics:?}"
    );
}

#[test]
fn jdk_instance_vararg_accepts_expanded_arguments() {
    const SOURCE: &str = "class Marker {\n\
    fun first(): String = \"O\"\n\
    fun join(left: String, right: String): String = left + right\n\
}\n\
fun box(): String {\n\
    val marker = Marker()\n\
    val first = Marker::class.java.getDeclaredMethod(\"first\").invoke(marker) as String\n\
    val join = Marker::class.java.getDeclaredMethod(\"join\", String::class.java, String::class.java)\n\
    return first + (join.invoke(marker, \"K\", \"\") as String)\n\
}\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main").expect("compile Java vararg calls"),
        "OK"
    );
}

#[test]
fn jdk_vararg_constructor_accepts_expanded_arguments() {
    const SOURCE: &str = "import java.lang.ProcessBuilder\n\
fun box(): String {\n\
    val values = ProcessBuilder(\"O\", \"K\").command()\n\
    return values[0] + values[1]\n\
}\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main")
            .expect("compile Java vararg constructor"),
        "OK"
    );
}

#[test]
fn fully_qualified_jdk_vararg_constructor_accepts_expanded_arguments() {
    const SOURCE: &str = "fun box(): String {\n\
    val size = java.lang.ProcessBuilder(null, \"K\").command().size\n\
    return if (size == 2) \"OK\" else size.toString()\n\
}\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main")
            .expect("compile fully qualified Java vararg constructor"),
        "OK"
    );
}

#[test]
fn function_value_is_adapted_to_java_sam() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let classpath = vec![java_classes, stdlib];
    let source = "import fixtures.NumericApi\n\
        fun box(): String {\n\
        \x20 val operation: (Int) -> Int = { it + 1 }\n\
        \x20 if (NumericApi.transform(1, operation) != 2) return \"static\"\n\
        \x20 if (NumericApi().transformInstance(2, operation) != 3) return \"instance\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "compile function-value SAM adaptation: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output =
        common::run_box(&classes, "MainKt", &classpath).expect("run function-value SAM adaptation");
    let _ = std::fs::remove_dir_all(temp_root);
    assert_eq!(output.trim(), "OK");
}

#[test]
fn unrelated_instance_sam_overloads_are_ambiguous() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let source = "import fixtures.NumericApi\nfun f(): Int = NumericApi().choose { 1 }\n";
    let diagnostics = common::front_end_diagnostics(source, &[java_classes], Some(jdk.as_path()));
    let _ = std::fs::remove_dir_all(temp_root);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("none of the following candidates is applicable")),
        "{diagnostics:?}"
    );
}

#[test]
fn implicit_parameter_lambda_does_not_match_a_multi_input_java_sam() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let source =
        "import fixtures.NumericApi\nfun f(): Int = NumericApi.consumeBoth { \"unused\" }\n";
    let diagnostics = common::front_end_diagnostics(source, &[java_classes], Some(jdk.as_path()));
    let _ = std::fs::remove_dir_all(temp_root);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static")),
        "{diagnostics:?}"
    );
}

#[test]
fn heterogeneous_generic_evidence_does_not_narrow_a_later_sam() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let source = "import fixtures.NumericApi\n\
        fun f(): String = NumericApi.consumePair(\"x\", Any()) { it.length }\n";
    let diagnostics = common::front_end_diagnostics(source, &[java_classes], Some(jdk.as_path()));
    let _ = std::fs::remove_dir_all(temp_root);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'length'")),
        "{diagnostics:?}"
    );
}

#[test]
fn incomparable_literal_overloads_are_ambiguous() {
    let jdk = common::jdk_modules();
    let Some((java_classes, temp_root)) = numeric_api() else {
        return;
    };
    let classpath = [java_classes];
    for call in [
        "NumericApi.cross(1, 1)",
        "NumericApi.incomparable(1, \"x\")",
    ] {
        let source = format!("import fixtures.NumericApi\nfun f(): String = {call}\n");
        let diagnostics = common::front_end_diagnostics(&source, &classpath, Some(jdk.as_path()));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("unresolved Java static")),
            "{call}: {diagnostics:?}"
        );
    }
    let source = "import fixtures.NumericApi\nfun f(): Int = NumericApi.supply { \"wrong\" }\n";
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static")),
        "{diagnostics:?}"
    );
    let source = "import fixtures.NumericApi\nfun f(): String = NumericApi.supplyText { 1 }\n";
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static")),
        "{diagnostics:?}"
    );
    let _ = std::fs::remove_dir_all(temp_root);
}

#[test]
fn top_level_library_calls_use_literal_origin_after_argument_mapping() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = common::compile_lib(
        "integer_literal_top_level",
        r#"
            package fixtures
            fun mixed(first: Int, second: Long): String = "$first:$second"
            fun pick(value: Int): String = "int"
            fun pick(value: Long): String = "long"
            fun onlyLong(value: Long): String = value.toString()
            fun inferred(value: Long): Long = value
            fun inferred(value: String): String = value
        "#,
    ) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.inferred
        import fixtures.mixed
        import fixtures.onlyLong
        import fixtures.pick

        private val inferredLong = inferred(4)

        fun box(): String {
            if (mixed(second = 2, first = 1) != "1:2") return "mixed"
            if (pick(1) != "int") return "pick"
            if (onlyLong(-3) != "-3") return "long"
            if (inferredLong != 4L) return "inferred"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "compile top-level interop: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run top-level interop");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn generic_java_static_accepts_zero_arg_sam_lambda() {
    const SOURCE: &str = "import java.lang.ThreadLocal\n\
        private val state: ThreadLocal<String?> = ThreadLocal.withInitial { null }\n\
        fun box(): String {\n\
            state.set(\"OK\")\n\
            return state.get() ?: \"fail\"\n\
        }\n";

    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main")
            .expect("generic Java static SAM call compiles and runs"),
        "OK"
    );
}

#[test]
fn jdk_static_call_return_type_inferred_for_private_property() {
    const SOURCE: &str = "import java.time.Instant\n\
        class Holder {\n\
            private val value = Instant.ofEpochSecond(1_700_000_000)\n\
            private val computed = Instant.ofEpochSecond(1_700_000_000 + 1)\n\
            fun epochSecond(): Long = value.epochSecond\n\
            fun computedEpochSecond(): Long = computed.epochSecond\n\
        }\n\
        fun box(): String {\n\
            if (Holder().epochSecond() != 1_700_000_000L) return \"wrong epoch\"\n\
            if (Holder().computedEpochSecond() != 1_700_000_001L) return \"wrong computed\"\n\
            return \"OK\"\n\
        }\n";

    let output = common::compile_and_run_with_stdlib(SOURCE, "Main").unwrap_or_else(|| {
        let stdlib = common::stdlib_jar();
        let jdk = common::jdk_modules();
        panic!(
            "{:?}",
            common::front_end_diagnostics(SOURCE, &[stdlib], Some(jdk.as_path()))
        )
    });
    assert_eq!(output.trim(), "OK");
}
