//! A declaration whose result is inferred from a Java generic static call that SAM-converts a Kotlin
//! function value. `ThreadLocal.withInitial(supplier)` with `supplier: () -> X` binds the Java
//! method's `S` through the converted `Supplier<out S>`, so the published declaration returns
//! `ThreadLocal<X>` rather than an unconstrained `ThreadLocal<*>`, and a caller may assign it to
//! `ThreadLocal<String>`.

use super::common;

fn assert_clean_diagnostic_ledger(label: &str, code: i32, stderr: &str, stdout: Option<&str>) {
    // `compiler_diagnostics` exposes krusty's stdout separately. Its recorded kotlinc boundary
    // exposes one diagnostic stream only, so the reference calls pass no stdout ledger.
    let stdout = stdout.unwrap_or_default();
    assert_eq!(
        (
            code,
            common::compiler_errors(stderr),
            common::compiler_warnings(stderr),
            common::compiler_module_warnings(stderr),
            common::compiler_errors(stdout),
            common::compiler_warnings(stdout),
            common::compiler_module_warnings(stdout),
        ),
        (
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        "{label} emitted diagnostics:\nstderr:\n{stderr}\nstdout:\n{stdout}",
    );
}

const DECLARATIONS: &str = r#"package sam

fun <X> perThread(supplier: () -> X) = ThreadLocal.withInitial(supplier)

class Holder<X>(supplier: () -> X) {
    val local = ThreadLocal.withInitial(supplier)
}
"#;

const USE: &str = r#"package sam

fun box(): String {
    val top: ThreadLocal<String> = perThread { "O" }
    val property: ThreadLocal<String> = Holder { "K" }.local
    val text: String = top.get() + property.get()
    return if (text == "OK") "OK" else "fail: $text"
}
"#;

#[test]
fn sam_converted_argument_binds_inferred_declaration_result_across_files() {
    let sources = [("Declarations.kt", DECLARATIONS), ("Use.kt", USE)];
    let compiled = common::compile_and_run_files_with_stdlib(&sources)
        .expect("SAM-converted Java static call infers the declaration result");
    assert_eq!(
        compiled,
        common::kotlinc_box_files_result(&sources, "sam.UseKt")
    );
    assert_eq!(compiled, "OK");
}

const SAME_FILE: &str = r#"package sam

class Factory {
    fun <X> perThread(supplier: () -> X) = ThreadLocal.withInitial(supplier)
}

class Holder<X>(supplier: () -> X) {
    val local = ThreadLocal.withInitial(supplier)
}

fun box(): String {
    val member: ThreadLocal<String> = Factory().perThread { "O" }
    val property: ThreadLocal<String> = Holder { "K" }.local
    val text: String = member.get() + property.get()
    return if (text == "OK") "OK" else "fail: $text"
}
"#;

#[test]
fn sam_converted_inferred_result_caller_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "JavaStaticSamInferredReturn",
        SAME_FILE,
        "sam/JavaStaticSamInferredReturnKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("sam/JavaStaticSamInferredReturnKt byte-identical to kotlinc");
}

#[test]
fn sam_parameter_and_result_constrain_inferred_result_by_variance() {
    let java = [(
        "Factory.java".to_string(),
        r#"
        package fixtures;

        import java.util.ArrayList;
        import java.util.function.UnaryOperator;

        public final class Factory {
            public static <T> ArrayList<T> make(UnaryOperator<T> operation) {
                return new ArrayList<>();
            }
        }
    "#
        .to_string(),
    )];
    let (library, _) = common::javac_compile(&java, &[])
        .expect("javac must compile the variance-sensitive SAM fixture");
    let root = library.parent().map(std::path::Path::to_path_buf);
    let source = r#"import fixtures.Factory

fun declaration(operation: (Number) -> Int) = Factory.make(operation)

fun use(operation: (Number) -> Int) {
    val exact: java.util.ArrayList<Int> = declaration(operation)
}
"#;

    let compiled = common::compiler_diagnostics(&[("main.kt", source)], &[library]);
    assert_clean_diagnostic_ledger(
        "kotlinc variance-sensitive SAM fixture",
        compiled.reference_code,
        &compiled.reference_stderr,
        None,
    );
    assert_clean_diagnostic_ledger(
        "krusty variance-sensitive SAM fixture",
        compiled.krusty_code,
        &compiled.krusty_stderr,
        Some(&compiled.krusty_stdout),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn sam_upper_bound_remains_separate_from_later_lower_evidence() {
    let java = [(
        "Factory.java".to_string(),
        r#"
        package fixtures;

        public final class Factory {
            @FunctionalInterface
            public interface Sink<T> {
                void accept(T value);
            }

            public static <T> T choose(Sink<T> sink, T value) {
                return value;
            }
        }
    "#
        .to_string(),
    )];
    let (library, _) = common::javac_compile(&java, &[])
        .expect("javac must compile the SAM upper/lower constraint fixture");
    let root = library.parent().map(std::path::Path::to_path_buf);
    let source = r#"import fixtures.Factory

fun declaration(sink: (Any) -> Unit) = Factory.choose(sink, "OK")

fun use(sink: (Any) -> Unit) {
    val exact: String = declaration(sink)
}
"#;

    let compiled = common::compiler_diagnostics(&[("main.kt", source)], &[library]);
    assert_clean_diagnostic_ledger(
        "kotlinc ordered SAM constraint fixture",
        compiled.reference_code,
        &compiled.reference_stderr,
        None,
    );
    assert_clean_diagnostic_ledger(
        "krusty ordered SAM constraint fixture",
        compiled.krusty_code,
        &compiled.krusty_stderr,
        Some(&compiled.krusty_stdout),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
}
