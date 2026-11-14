//! PLATFORM: JVM A lambda literal takes the position of the nearest call enclosing it, as
//! kotlinc's `PatchLambdaOffsetsLowering` moves it: the lambda's instance is created on the call's
//! line, not the line the lambda opens on. The call is the nearest one however deep the lambda sits
//! in its arguments (through a constructor call or a conditional), a property setter, an operator
//! `set` call and an array factory included. A direct field read is not a call. A variable's
//! initializer, a local function's body and an inline lambda's body are declarations, under which
//! a lambda keeps its own line, as do an anonymous function and a lambda no call encloses.

use super::common;

const SOURCE: &str = include_str!("box_regressions/LambdaArgumentCallLine.kt");

#[test]
fn lambda_argument_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("LambdaArgumentCallLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn lambda_arguments_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "LambdaArgumentCallLine");
}

// PLATFORM-SPECIFIC: This complete counterprogram imports a Java field holder and checks JVM class artifacts.
const JAVA_FIELD: &str = "\
package fixtures;
import kotlin.jvm.functions.Function0;
public final class JBox {
    public final Object value;
    public JBox(Function0<kotlin.Unit> value) { this.value = value; }
}
";

/// A Java field read is `IrGetField`, not an `IrCall`, so a lambda in its receiver keeps the
/// enclosing call's line. The getter cases in the fixture above stay on the accessor call.
const FIELD_RECEIVER: &str = "\
import fixtures.JBox
private val log = StringBuilder()
fun <T> pass(value: T): T = value
fun directField(x: String): Any = pass(
    JBox({ log.append(x) }).value
)
fun box(): String {
    val value = directField(\"OK\")
    (value as () -> Any?)()
    return log.toString()
}
";

#[test]
fn lambda_in_a_java_field_receiver_stays_on_the_enclosing_call() {
    let (java_dir, _) = common::javac_compile(
        &[("JBox.java".to_string(), JAVA_FIELD.to_string())],
        &[common::stdlib_jar()],
    )
    .expect("javac compiles the field holder");
    let sources = [("DirectFieldLambda.kt", FIELD_RECEIVER)];
    let classes =
        common::classes_against_kotlinc_module_cp(&sources, std::slice::from_ref(&java_dir));
    let jdk = common::jdk_modules();
    let mut classpath = common::classpath_jars_for(FIELD_RECEIVER);
    classpath.push(java_dir.clone());
    let ran = common::compile_and_run_box(
        FIELD_RECEIVER,
        "DirectFieldLambda",
        &classpath,
        Some(jdk.as_path()),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(classes.differences(), Vec::<String>::new());
    assert_eq!(ran.as_deref(), Some("OK"));
}
