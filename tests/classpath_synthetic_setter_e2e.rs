//! Java synthetic-property WRITES with a hierarchy-overridden setter: when a JavaBean `setX` is
//! declared at several rungs of the receiver's supertype chain (e.g. `JComponent.setFont`,
//! `Container.setFont`, `Component.setFont`), kotlinc still synthesizes the mutable property and
//! binds the MOST-DERIVED override. krusty used to count every override as a separate candidate and
//! refuse the "ambiguous" set, misreporting `'val' cannot be reassigned.` on a legal write.

use super::common;

/// Minimal repro with a javac fixture: `Sub.setSize` overrides `Base.setSize`, so a `Sub` receiver
/// sees the setter at two rungs. The write must resolve (to the most-derived override) and the
/// read-back must observe it.
#[test]
fn overridden_java_setter_accepts_synthetic_property_write() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let base = "package fixtures;\n\
        public class Base {\n\
        \x20 private int size;\n\
        \x20 public int getSize() { return size; }\n\
        \x20 public void setSize(int s) { size = s; }\n\
        }\n";
    let sub = "package fixtures;\n\
        public class Sub extends Base {\n\
        \x20 @Override public void setSize(int s) { super.setSize(s); }\n\
        }\n";
    let Some((classes, _root)) = common::javac_compile(
        &[
            ("Base.java".into(), base.into()),
            ("Sub.java".into(), sub.into()),
        ],
        &[],
    ) else {
        return;
    };
    let source = "import fixtures.Sub\n\
        fun box(): String {\n\
        \x20 val s = Sub()\n\
        \x20 s.size = 3\n\
        \x20 return if (s.size == 3) \"OK\" else \"fail\"\n\
        }\n";
    let classpath = vec![classes, stdlib];
    assert_eq!(
        common::expect_box_run(source, "Main", &classpath, Some(jdk.as_path())),
        "OK"
    );
}

/// The original find: `javax.swing.JLabel.font` — `setFont` is declared on `JComponent`,
/// `Container`, AND `Component` (three rungs). kotlinc accepts `label.font = …`; krusty reported
/// `'val' cannot be reassigned.`
#[test]
fn swing_font_write_uses_hierarchy_overridden_setter() {
    const SOURCE: &str = "import javax.swing.JLabel\n\
        import java.awt.Font\n\
        fun box(): String {\n\
        \x20 val label = JLabel(\"x\")\n\
        \x20 label.font = Font(\"Dialog\", 0, 12)\n\
        \x20 return if (label.font.size == 12) \"OK\" else \"fail\"\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "Main").expect("toolchain provisioned"),
        "OK"
    );
}
