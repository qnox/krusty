//! A Java constructor's class type parameters are its result variables. An invariant expected
//! type therefore widens an argument-inferred binding when the argument still fits, the same way
//! a Kotlin constructor already does. A constructor-declared type parameter stays a separate
//! variable, including when it reuses the class parameter's name. A Kotlin constructor still
//! rejects an argument that does not fit the expected binding.

use super::common;

#[test]
fn java_constructor_widens_to_the_expected_class_type_argument() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Cell.java".into(),
            r#"
                package fixtures;
                public final class Cell<E> {
                    public final E value;
                    public Cell(E value) { this.value = value; }
                    public <U> Cell(E value, U extra) { this.value = value; }
                }
            "#
            .into(),
        ),
        (
            "Shadow.java".into(),
            r#"
                package fixtures;
                public final class Shadow<T> {
                    public final Object value;
                    public <T> Shadow(T value) { this.value = value; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac rejected the constructor fixtures");
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    const SOURCE: &str = r#"
        import fixtures.Cell
        import fixtures.Shadow
        import java.util.ArrayList
        import java.util.concurrent.atomic.AtomicReference

        fun widenedNullable(s: String): AtomicReference<String?> = AtomicReference(s)
        fun widenedAny(s: String): AtomicReference<Any> = AtomicReference(s)
        fun widenedCharSequence(s: String): AtomicReference<CharSequence> = AtomicReference(s)
        fun nullableArgument(s: String?): AtomicReference<String?> = AtomicReference(s)
        fun platform(s: String?): AtomicReference<String> = AtomicReference(s)
        fun listed(s: String): ArrayList<CharSequence> = ArrayList(listOf(s))
        fun cell(s: String): Cell<CharSequence> = Cell(s)
        fun cellExtra(s: String): Cell<Any> = Cell(s, 1)
        fun shadow(s: String): Shadow<CharSequence> = Shadow(s)

        fun box(): String {
            if (widenedNullable("a").get() != "a") return "nullable"
            if (widenedAny("b").get() != "b") return "any"
            if (widenedCharSequence("c").get() != "c") return "charseq"
            if (nullableArgument("d").get() != "d") return "arg"
            if (platform(null).get() != null) return "platform"
            if (listed("e")[0] != "e") return "list"
            if (cell("f").value != "f") return "cell"
            if (cellExtra("g").value != "g") return "extra"
            if (shadow("h").value != "h") return "shadow"
            return "OK"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result_with_args(
        "JavaConstructorExpectedType",
        SOURCE,
        &[
            "-cp".to_string(),
            library.to_string_lossy().into_owned(),
            "-nowarn".to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "kotlinc rejected the constructor fixtures: {diagnostics}"
    );
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()));
    assert!(
        krusty.is_empty(),
        "a Java constructor must take its class type arguments from the expected result: {krusty:?}"
    );
    let output = common::compile_and_run_box(
        SOURCE,
        "JavaConstructorExpectedType",
        &classpath,
        Some(jdk.as_path()),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.as_deref().map(str::trim), Some("OK"));
}

#[test]
fn kotlin_constructor_does_not_widen_past_an_argument_that_does_not_fit() {
    const SOURCE: &str = r#"
        class Box<T>(val value: T)
        fun rejected(s: String?): Box<String> = Box(s)
    "#;
    let (code, _) = common::kotlinc_source_result("KotlinConstructorExpectedMismatch", SOURCE);
    assert_ne!(code, 0, "kotlinc must reject Box<String>(nullable)");
    let stdlib = common::stdlib_jar();
    let krusty = common::front_end_diagnostics(SOURCE, std::slice::from_ref(&stdlib), None);
    assert_eq!(
        krusty,
        vec!["return type mismatch: expected 'Box<String>', actual 'Box<String?>'.".to_string()]
    );
}
