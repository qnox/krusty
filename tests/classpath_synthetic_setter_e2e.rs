//! Java synthetic-property WRITES with a hierarchy-overridden setter: when a JavaBean `setX` is
//! declared at several rungs of the receiver's supertype chain, kotlinc still synthesizes the
//! mutable property and binds the MOST-DERIVED override. krusty used to count every override as a
//! separate candidate and refuse the "ambiguous" set, misreporting `'val' cannot be reassigned.` on
//! a legal write.

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

/// A getter and setter that merely share a bean NAME do not form a Kotlin synthetic `var`: their
/// value types must agree. Keep this next to the override regression because selecting the nearest
/// setter only AFTER validating the getter/setter pair is the semantic boundary. In particular, a
/// lone mismatched setter must not bypass the type filter just because there is no overload to
/// disambiguate against; doing so makes the checker invent a writable `Int` property whose read type
/// is `String`.
#[test]
fn mismatched_java_getter_and_setter_do_not_form_a_mutable_property() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package fixtures;\n\
        public class Mismatch {\n\
        \x20 public String getValue() { return \"read\"; }\n\
        \x20 public void setValue(int value) {}\n\
        }\n";
    let Some((classes, _root)) =
        common::javac_compile(&[("Mismatch.java".into(), source.into())], &[])
    else {
        return;
    };
    let consumer = "import fixtures.Mismatch\n\
        fun write(target: Mismatch) {\n\
        \x20 target.value = 7\n\
        }\n";
    let diagnostics =
        common::front_end_diagnostics(consumer, &[classes, stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("cannot be reassigned")),
        "a mismatched JavaBean pair must remain read-only: {diagnostics:?}"
    );
}

/// Setter discovery must obey the same access context as an ordinary member call. A public getter
/// with a protected setter is a readable synthetic property, but not a writable one outside the Java
/// declaration. Keeping the inaccessible setter in the candidate set would make checking succeed
/// and leave the JVM to reject the emitted call with `IllegalAccessError`.
#[test]
fn inaccessible_java_setter_does_not_form_a_mutable_property() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package fixtures;\n\
        public class ReadOnly {\n\
        \x20 private int value;\n\
        \x20 public int getValue() { return value; }\n\
        \x20 protected void setValue(int value) { this.value = value; }\n\
        }\n";
    let Some((classes, _root)) =
        common::javac_compile(&[("ReadOnly.java".into(), source.into())], &[])
    else {
        return;
    };
    let consumer = "import fixtures.ReadOnly\n\
        fun write(target: ReadOnly) {\n\
        \x20 target.value = 7\n\
        }\n";
    let diagnostics =
        common::front_end_diagnostics(consumer, &[classes, stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("cannot be reassigned")),
        "an inaccessible Java setter must leave the synthetic property read-only: {diagnostics:?}"
    );
}
