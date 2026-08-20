//! Java arrays are covariant, and Kotlin models a Java array slot as `Array<(out) T!>!`.
//! A `Sub[]` therefore reaches a `Base[]` parameter, while Kotlin's own `Array<T>` stays invariant.

use super::common;

#[test]
fn a_java_array_parameter_accepts_a_subtype_element_array() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Base.java".into(),
            r#"
                package fixtures;
                public class Base {
                    public final String tag;
                    public Base(String tag) { this.tag = tag; }
                }
            "#
            .into(),
        ),
        (
            "Sub.java".into(),
            r#"
                package fixtures;
                public class Sub extends Base {
                    public Sub(String tag) { super(tag); }
                    public static Sub[] parse(String tags) {
                        String[] parts = tags.split(",");
                        Sub[] subs = new Sub[parts.length];
                        for (int i = 0; i < parts.length; i++) { subs[i] = new Sub(parts[i]); }
                        return subs;
                    }
                }
            "#
            .into(),
        ),
        (
            "Sink.java".into(),
            r#"
                package fixtures;
                public class Sink {
                    public String seen = "";
                    public void set(String kind, Base[] items) {
                        StringBuilder out = new StringBuilder(kind);
                        for (Base item : items) { out.append(':').append(item.tag); }
                        seen = out.toString();
                    }
                }
            "#
            .into(),
        ),
    ];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the covariance fixture");
    let root = library.parent().map(std::path::Path::to_path_buf);
    const SOURCE: &str = r#"
        import fixtures.Sink
        import fixtures.Sub

        fun box(): String {
            val sink = Sink()
            sink.set("to", Sub.parse("a,b"))
            return if (sink.seen == "to:a:b") "OK" else sink.seen
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result_with_args(
        "JavaArrayCovariance",
        SOURCE,
        &[
            "-cp".to_string(),
            library.to_string_lossy().into_owned(),
            "-nowarn".to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    assert_eq!(diagnostics, "");
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()));
    assert_eq!(krusty, Vec::<String>::new());
    let output = common::compile_and_run_box(
        SOURCE,
        "JavaArrayCovariance",
        &classpath,
        Some(jdk.as_path()),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        output.as_deref().map(str::trim),
        Some("OK"),
        "the compiled call must run against the Java fixture"
    );
}

#[test]
fn a_kotlin_array_parameter_stays_invariant() {
    // The covariance belongs to the JAVA spelling. A Kotlin `Array<Base>` parameter still rejects an
    // `Array<Sub>`, exactly as kotlinc does — otherwise the store into it would be unchecked.
    const SOURCE: &str = r#"
        open class Base(val tag: String)

        class Sub(tag: String) : Base(tag)

        fun take(items: Array<Base>) {}

        fun use() {
            val subs: Array<Sub> = arrayOf(Sub("a"))
            take(subs)
        }
    "#;
    let (code, _) = common::kotlinc_source_result("KotlinArrayInvariance", SOURCE);
    assert_ne!(code, 0, "kotlinc must reject Array<Sub> for Array<Base>");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty =
        common::front_end_diagnostics(SOURCE, std::slice::from_ref(&stdlib), Some(jdk.as_path()));
    assert_eq!(
        krusty,
        ["argument type mismatch: actual type is 'Array<Sub>', but 'Array<Base>' was expected."]
    );
}

/// Shared control+run for the `orElse(emptyArray())` shapes below: kotlinc must accept the source
/// silently, krusty's front end must report nothing, and the compiled box must run.
fn assert_java_optional_or_else_empty_array(tag: &str, use_src: &str) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "J.java".into(),
        r#"
            import java.util.Optional;
            public class J {
                public static Optional<String[]> args() { return Optional.of(new String[]{"x"}); }
            }
        "#
        .into(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the optional-array fixture");
    let root = library.parent().map(std::path::Path::to_path_buf);
    let (code, diagnostics) = common::kotlinc_source_result_with_args(
        tag,
        use_src,
        &[
            "-cp".to_string(),
            library.to_string_lossy().into_owned(),
            "-nowarn".to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    assert_eq!(diagnostics, "");
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(use_src, &classpath, Some(jdk.as_path()));
    assert_eq!(krusty, Vec::<String>::new());
    let output = common::compile_and_run_box(use_src, tag, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        output.as_deref().map(str::trim),
        Some("OK"),
        "the compiled call must run against the Java fixture"
    );
}

#[test]
fn empty_array_argument_uses_java_array_out_projection_element_type() {
    // A Java `String[]` maps to the flexible, out-projected `Array<out String!>!`. The
    // `Optional.orElse` parameter fixes the element type because an empty array contributes no
    // argument evidence of its own.
    assert_java_optional_or_else_empty_array(
        "JavaOrElseEmptyArrayProjection",
        r#"
            fun box(): String {
                val r = J.args().orElse(emptyArray())
                return if (r.size == 1 && r[0] == "x") "OK" else "FAIL"
            }
        "#,
    );
}

#[test]
fn empty_array_spread_after_java_or_else_matches_kotlinc() {
    // The projected array read contributes String to the surrounding `listOf` call. This remains a
    // front-end test because mixed value-plus-spread vararg lowering is not implemented.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "J.java".into(),
        r#"
            import java.util.Optional;
            public class J {
                public static Optional<String[]> args() { return Optional.of(new String[]{"x"}); }
            }
        "#
        .into(),
    )];
    let (library, _) = common::javac_compile(&java, &[])
        .expect("javac must compile the optional-array spread fixture");
    let root = library.parent().map(std::path::Path::to_path_buf);
    const SOURCE: &str = r#"
        fun box(): String {
            val args: List<String> = listOf("first", *J.args().orElse(emptyArray()))
            return if (args == listOf("first", "x")) "OK" else "FAIL"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result_with_args(
        "JavaOrElseEmptyArraySpread",
        SOURCE,
        &[
            "-cp".to_string(),
            library.to_string_lossy().into_owned(),
            "-nowarn".to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    assert_eq!(diagnostics, "");
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(krusty, Vec::<String>::new());
}
