//! SAM conversion of a bare lambda argument into a Java CONSTRUCTOR parameter. The method-call
//! overload selector adapts a lambda literal against a `sam_method`-carrying parameter, but
//! classpath constructor matching dropped that adaptation, so every candidate looked inapplicable
//! and the call fell through to `unresolved function '<ClassName>'`. The neutral fixtures exercise
//! both bare/imported and fully-qualified construction; trailing and parenthesized lambdas must RUN,
//! with the stored listener fired through the Java class and the Kotlin-side flag observed.
use super::common;

fn fixture_jar() -> Option<std::path::PathBuf> {
    let java = [
        (
            "EventSource.java".into(),
            r#"
                package fixtures;
                public final class EventSource {
                    private final String tip;
                    public EventSource(String tip) { this.tip = tip; }
                    public String getTip() { return tip; }
                }
            "#
            .into(),
        ),
        (
            "ListenerHolder.java".into(),
            r#"
                package fixtures;
                import java.awt.event.ActionListener;
                public class ListenerHolder {
                    private final EventSource source;
                    private final ActionListener listener;
                    public ListenerHolder(EventSource source, ActionListener listener) {
                        this.source = source;
                        this.listener = listener;
                    }
                    public void fire() {
                        listener.actionPerformed(new java.awt.event.ActionEvent(this, 0, "fire"));
                    }
                    public EventSource getSource() { return source; }
                }
            "#
            .into(),
        ),
        (
            "OverloadedListenerHolder.java".into(),
            r#"
                package fixtures;
                import java.awt.event.ActionListener;
                public class OverloadedListenerHolder {
                    private final ActionListener listener;
                    private final String tag;
                    public OverloadedListenerHolder(EventSource source, ActionListener listener) {
                        this.listener = listener;
                        this.tag = "two";
                    }
                    public OverloadedListenerHolder(String label, EventSource source, ActionListener listener) {
                        this.listener = listener;
                        this.tag = "three:" + label;
                    }
                    public void fire() {
                        listener.actionPerformed(new java.awt.event.ActionEvent(this, 0, "fire"));
                    }
                    public String getTag() { return tag; }
                }
            "#
            .into(),
        ),
        (
            "TextOnly.java".into(),
            r#"
                package fixtures;
                public class TextOnly {
                    public TextOnly(String label) {}
                }
            "#
            .into(),
        ),
    ];
    let (library, _) = common::javac_compile(&java, &[])?;
    Some(library)
}

fn run_box(source: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = fixture_jar()?;
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath);
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    output
}

#[test]
fn java_ctor_trailing_lambda_sam_conversion_runs() {
    let source = r#"
        import fixtures.EventSource
        import fixtures.ListenerHolder

        fun box(): String {
            var fired = false
            val icons = EventSource("close")
            val button = ListenerHolder(icons) {
                fired = true
            }
            if (button.source.tip != "close") return "source"
            button.fire()
            return if (fired) "OK" else "not fired"
        }
    "#;
    assert_eq!(
        run_box(source).expect("trailing-lambda ctor SAM conversion"),
        "OK"
    );
}

#[test]
fn java_ctor_parenthesized_lambda_sam_conversion_runs() {
    let source = r#"
        import fixtures.EventSource
        import fixtures.ListenerHolder

        fun box(): String {
            var fired = false
            val icons = EventSource("close")
            val button = ListenerHolder(icons, { fired = true })
            button.fire()
            return if (fired) "OK" else "not fired"
        }
    "#;
    assert_eq!(
        run_box(source).expect("parenthesized-lambda ctor SAM conversion"),
        "OK"
    );
}

#[test]
fn java_ctor_lambda_sam_conversion_picks_matching_overload() {
    let source = r#"
        import fixtures.EventSource
        import fixtures.OverloadedListenerHolder

        fun box(): String {
            var fired = 0
            val icons = EventSource("close")
            val two = OverloadedListenerHolder(icons) { fired += 1 }
            val three = OverloadedListenerHolder("x", icons) { fired += 10 }
            if (two.tag != "two") return "two:${two.tag}"
            if (three.tag != "three:x") return "three:${three.tag}"
            two.fire()
            three.fire()
            return if (fired == 11) "OK" else "fired=$fired"
        }
    "#;
    assert_eq!(
        run_box(source).expect("overload disambiguation with SAM lambda"),
        "OK"
    );
}

/// A lambda whose checking is DEFERRED to selection binds `it` to the SAM method's parameter type
/// on the first check — an expectation-free pass would bind it as `Any` and record `unresolved
/// reference 'paramString'` before the conversion was known (the method-call path defers the same
/// way).
#[test]
fn java_ctor_lambda_implicit_it_binds_sam_parameter_type() {
    let source = r#"
        import fixtures.EventSource
        import fixtures.ListenerHolder

        fun box(): String {
            var seen = ""
            val icons = EventSource("close")
            val button = ListenerHolder(icons) { seen = it.paramString() }
            button.fire()
            return if (seen.contains("fire")) "OK" else "seen=$seen"
        }
    "#;
    assert_eq!(
        run_box(source).expect("implicit-it SAM parameter binding"),
        "OK"
    );
}

#[test]
fn java_ctor_lambda_declared_parameter_binds_sam_parameter_type() {
    let source = r#"
        import fixtures.EventSource
        import fixtures.ListenerHolder

        fun box(): String {
            var seen = ""
            val icons = EventSource("close")
            val button = ListenerHolder(icons) { e -> seen = e.paramString() }
            button.fire()
            return if (seen.contains("fire")) "OK" else "seen=$seen"
        }
    "#;
    assert_eq!(
        run_box(source).expect("declared-parameter SAM parameter binding"),
        "OK"
    );
}

/// Qualified and imported constructor spellings must share the same post-selection lambda check.
/// This specifically pins the generic constructor boundary: neither syntax may eagerly type the
/// lambda and bind its parameter as `Any` before the selected SAM method supplies `ActionEvent`.
#[test]
fn qualified_java_ctor_lambda_binds_the_selected_sam_parameter_type() {
    let source = r#"
        fun box(): String {
            var seen = ""
            val source = fixtures.EventSource("close")
            val holder = fixtures.ListenerHolder(source) { seen = it.paramString() }
            holder.fire()
            return if (seen.contains("fire")) "OK" else "seen=$seen"
        }
    "#;
    assert_eq!(
        run_box(source).expect("qualified constructor SAM parameter binding"),
        "OK"
    );
}

/// The deferred lambda is checked exactly ONCE (against the selected SAM parameter): a body error
/// must be reported a single time, not once per pass.
#[test]
fn java_ctor_lambda_body_error_reports_exactly_once() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = fixture_jar() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.EventSource
        import fixtures.ListenerHolder

        fun mk(): ListenerHolder {
            val icons = EventSource("close")
            return ListenerHolder(icons) { noSuchFunction() }
        }
    "#;
    let diags = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    let occurrences = diags
        .iter()
        .filter(|d| d.contains("unresolved reference 'noSuchFunction'."))
        .count();
    assert_eq!(occurrences, 1, "expected exactly one report, got {diags:?}");
}

/// A lambda cannot convert to a non-SAM parameter: `TextOnly(String)` called with a trailing lambda
/// must still fail resolution with the same argument mismatch as kotlinc. No enclosing return-type
/// cascade is valid once the construction expression has already failed.
#[test]
fn java_ctor_lambda_against_non_sam_parameter_still_fails() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = fixture_jar() else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.TextOnly

        fun mk(): TextOnly {
            return TextOnly { }
        }
    "#;
    let diags = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        diags,
        ["argument type mismatch: actual type is '() -> Unit', but 'String!' was expected."]
    );
}
