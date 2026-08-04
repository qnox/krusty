//! SAM conversion of a bare lambda argument into a Java CONSTRUCTOR parameter. The method-call
//! overload selector adapts a lambda literal against a `sam_method`-carrying parameter, but
//! classpath constructor matching dropped that adaptation, so every candidate looked inapplicable
//! and the call fell through to `unresolved function '<ClassName>'` (intellij-community's
//! `BannerStartPagePromoter.kt`: `InplaceButton(icons) { closeAction() }`). kotlinc accepts all
//! shapes; the trailing-lambda and parenthesized forms below must also RUN — the stored listener is
//! fired through the Java class and the Kotlin-side flag observed.
use super::common;

fn fixture_jar() -> Option<std::path::PathBuf> {
    let java = [
        (
            "IconButton.java".into(),
            r#"
                package fixtures;
                public final class IconButton {
                    private final String tip;
                    public IconButton(String tip) { this.tip = tip; }
                    public String getTip() { return tip; }
                }
            "#
            .into(),
        ),
        (
            "InplaceButton.java".into(),
            r#"
                package fixtures;
                import java.awt.event.ActionListener;
                public class InplaceButton {
                    private final IconButton source;
                    private final ActionListener listener;
                    public InplaceButton(IconButton source, ActionListener listener) {
                        this.source = source;
                        this.listener = listener;
                    }
                    public void fire() {
                        listener.actionPerformed(new java.awt.event.ActionEvent(this, 0, "fire"));
                    }
                    public IconButton getSource() { return source; }
                }
            "#
            .into(),
        ),
        (
            "OverloadedButton.java".into(),
            r#"
                package fixtures;
                import java.awt.event.ActionListener;
                public class OverloadedButton {
                    private final ActionListener listener;
                    private final String tag;
                    public OverloadedButton(IconButton source, ActionListener listener) {
                        this.listener = listener;
                        this.tag = "two";
                    }
                    public OverloadedButton(String label, IconButton source, ActionListener listener) {
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
            "Widget.java".into(),
            r#"
                package fixtures;
                public class Widget {
                    public Widget(String label) {}
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
        import fixtures.IconButton
        import fixtures.InplaceButton

        fun box(): String {
            var fired = false
            val icons = IconButton("close")
            val button = InplaceButton(icons) {
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
        import fixtures.IconButton
        import fixtures.InplaceButton

        fun box(): String {
            var fired = false
            val icons = IconButton("close")
            val button = InplaceButton(icons, { fired = true })
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
        import fixtures.IconButton
        import fixtures.OverloadedButton

        fun box(): String {
            var fired = 0
            val icons = IconButton("close")
            val two = OverloadedButton(icons) { fired += 1 }
            val three = OverloadedButton("x", icons) { fired += 10 }
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
/// way; intellij-community shapes use the event).
#[test]
fn java_ctor_lambda_implicit_it_binds_sam_parameter_type() {
    let source = r#"
        import fixtures.IconButton
        import fixtures.InplaceButton

        fun box(): String {
            var seen = ""
            val icons = IconButton("close")
            val button = InplaceButton(icons) { seen = it.paramString() }
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
        import fixtures.IconButton
        import fixtures.InplaceButton

        fun box(): String {
            var seen = ""
            val icons = IconButton("close")
            val button = InplaceButton(icons) { e -> seen = e.paramString() }
            button.fire()
            return if (seen.contains("fire")) "OK" else "seen=$seen"
        }
    "#;
    assert_eq!(
        run_box(source).expect("declared-parameter SAM parameter binding"),
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
        import fixtures.IconButton
        import fixtures.InplaceButton

        fun mk(): InplaceButton {
            val icons = IconButton("close")
            return InplaceButton(icons) { noSuchFunction() }
        }
    "#;
    let diags = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    let occurrences = diags
        .iter()
        .filter(|d| d.contains("unresolved function 'noSuchFunction'"))
        .count();
    assert_eq!(occurrences, 1, "expected exactly one report, got {diags:?}");
}

/// A lambda cannot convert to a non-SAM parameter: `Widget(String)` called with a trailing lambda
/// must still fail resolution. (kotlinc reports `argument type mismatch: actual type is '() ->
/// Unit', but 'String!' was expected`; krusty's constructor path reports an inapplicable call as
/// unresolved — this pins that resolution still fails, in krusty's existing shape.)
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
        import fixtures.Widget

        fun mk(): Widget {
            return Widget { }
        }
    "#;
    let diags = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved function 'Widget'")),
        "expected unresolved-constructor diagnostic, got {diags:?}"
    );
}
