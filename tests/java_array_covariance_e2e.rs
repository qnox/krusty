//! Java arrays are covariant, and Kotlin models a Java array slot as `Array<(out) T!>!`. A
//! `Sub[]` therefore reaches a `Base[]` parameter — the shape behind `MimeMessage.setRecipients(…,
//! InternetAddress.parse(to))`, which krusty reported as an unresolved reference because no
//! candidate accepted the argument. Kotlin's OWN `Array<T>` stays invariant.

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
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
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
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()));
    assert!(
        krusty.is_empty(),
        "a Java array parameter must accept a subtype element array: {krusty:?}"
    );
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
    let krusty = common::front_end_diagnostics_with_stdlib(SOURCE);
    assert!(
        krusty
            .iter()
            .any(|diagnostic| diagnostic.contains("type mismatch")),
        "a Kotlin array parameter must stay invariant: {krusty:?}"
    );
}
