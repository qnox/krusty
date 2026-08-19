use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_no_diagnostics(diagnostics: Vec<String>) {
    assert_eq!(
        diagnostics.len(),
        0,
        "unexpected diagnostics: {diagnostics:?}"
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

fn assert_checker_clean(src: &str) {
    assert_no_diagnostics(
        common::checker_diags_with_stdlib(src).expect("checker diagnostics available"),
    );
}

#[test]
fn java_runnable_sam_lambda_runs() {
    const SRC: &str = "import java.lang.Runnable\n\
fun box(): String {\n\
    var s = \"\"\n\
    val r = Runnable { s = \"OK\" }\n\
    r.run()\n\
    return s\n\
}\n";
    assert_eq!(run(SRC).expect("Runnable SAM lambda compiles + runs"), "OK");
}

#[test]
fn classpath_sam_bridge_for_comparable_runs() {
    const SRC: &str = "class C : Comparable<C> {\n\
    override fun compareTo(other: C): Int = 7\n\
}\n\
fun box(): String {\n\
    val c: Comparable<C> = C()\n\
    return if (c.compareTo(C()) == 7) \"OK\" else \"no\"\n\
}\n";
    assert_eq!(run(SRC).expect("Comparable bridge compiles + runs"), "OK");
}

#[test]
fn java_static_sam_lambda_return_label_runs() {
    const LIB: &str = "import javax.swing.SwingUtilities\n\
fun javaStaticResult(): String {\n\
    var hit = \"\"\n\
    val x: String? = null\n\
    SwingUtilities.invokeAndWait {\n\
        val y = x ?: return@invokeAndWait\n\
        hit = y\n\
    }\n\
    return if (hit.isEmpty()) \"OK\" else \"no\"\n\
}\n";
    const MAIN: &str = "fun box(): String = javaStaticResult()\n";
    assert_checker_clean(LIB);
    assert_eq!(
        common::expect_box_run_against("java-static-sam-return-label", LIB, MAIN)
            .expect("both compilers run"),
        "OK"
    );
}

#[test]
fn classifier_value_sam_lambda_return_label_runs() {
    const LIB: &str = "fun interface Action { fun run() }\n\
object Runner {\n\
    fun runAction(action: Action) { action.run() }\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
    var hit = \"\"\n\
    val x: String? = null\n\
    Runner.runAction({\n\
        val y = x ?: return@runAction\n\
        hit = y\n\
    })\n\
    return if (hit.isEmpty()) \"OK\" else \"no\"\n\
}\n";
    assert_no_diagnostics(
        common::checker_diags_against("classifier-value-sam-return-label", LIB, MAIN)
            .expect("checker diagnostics available"),
    );
    assert_eq!(
        common::expect_box_run_against("classifier-value-sam-return-label", LIB, MAIN)
            .expect("both compilers run"),
        "OK"
    );
}

/// A functional-interface PARAMETER with no arguments (`Executor.execute(Runnable)`) is a real lambda
/// shape whose parameter list is empty — which is not the same as having no shape at all. A provider
/// that conflates the two drops the slot, and the lambda argument no longer matches the parameter.
#[test]
fn zero_argument_functional_interface_parameter_of_a_member_accepts_a_lambda() {
    let src = "import java.util.concurrent.Executors\n\
        fun box(): String {\n\
        \x20 val pool = Executors.newSingleThreadExecutor()\n\
        \x20 pool.execute { }\n\
        \x20 pool.shutdown()\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(output) = common::compile_and_run_with_stdlib(src, "Main") {
        assert_eq!(output.trim(), "OK");
    }
}
