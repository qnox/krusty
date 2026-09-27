//! A receiver lambda's extension receiver is named in the local-variable table after the label
//! `this@label` binds: the literal's own label, else the call the lambda is an argument of
//! (`$this$builder`). A lambda that is neither keeps kotlinc's `<this>`.

use super::common;

const SRC: &str = "class Controller {\n\
    fun step(n: Int): Int = n\n\
}\n\
class Host {\n\
    fun member(c: suspend Controller.() -> Unit) {}\n\
}\n\
suspend fun pause() {}\n\
fun builder(c: suspend Controller.() -> Unit) {}\n\
fun plain(c: Controller.() -> Unit) {}\n\
fun use(host: Host) {\n\
    builder { step(1); pause() }\n\
    builder named@{ step(2); pause() }\n\
    host.member { step(3); pause() }\n\
    plain { step(4) }\n\
}\n";

fn expect_method_matches(class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc("ReceiverLabels", &[], SRC, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const INVOKE_SUSPEND: &str = "public final java.lang.Object invokeSuspend(";

#[test]
fn a_suspend_lambda_argument_names_its_receiver_after_the_call() {
    expect_method_matches("ReceiverLabelsKt$use$1", INVOKE_SUSPEND);
}

#[test]
fn a_labelled_suspend_lambda_names_its_receiver_after_its_label() {
    expect_method_matches("ReceiverLabelsKt$use$2", INVOKE_SUSPEND);
}

#[test]
fn a_suspend_lambda_passed_to_a_member_names_its_receiver_after_the_member() {
    expect_method_matches("ReceiverLabelsKt$use$3", INVOKE_SUSPEND);
}

#[test]
fn a_plain_receiver_lambda_names_its_receiver_after_the_call() {
    expect_method_matches(
        "ReceiverLabelsKt",
        "private static final kotlin.Unit use$lambda$0(",
    );
}

// kotlinc labels a lambda literal after the innermost call it is written in, through `if`, `when`,
// `try`, parentheses and an enclosing lambda's body. A property initializer, an assignment and a
// non-infix operator name nothing; an infix call names itself; a call on a call's value is
// `invoke`.
const SHAPES: &str = "class C {\n\
    fun s(n: Int): Int = n\n\
}\n\
fun b(c: C.() -> Unit) {}\n\
fun <T> id(t: T): T = t\n\
fun <R> run2(f: () -> R): R = f()\n\
infix fun Int.with2(c: C.() -> Unit) {}\n\
class Box(val c: C.() -> Unit)\n\
fun getB(): (C.() -> Unit) -> Unit = { }\n\
var slot: C.() -> Unit = {}\n\
fun use(f: Boolean, n: (C.() -> Unit)?, v: (C.() -> Unit) -> Unit) {\n\
    b(if (f) { { s(1) } } else { { s(2) } })\n\
    b(when { f -> { { s(3) } } else -> { { s(4) } } })\n\
    b(try { { s(5) } } finally { })\n\
    b((({ s(6) })))\n\
    b(id { s(7) })\n\
    b(run2 { { s(8) } })\n\
    b(run2 { val x: C.() -> Unit = { s(9) }; x })\n\
    b(n ?: { s(10); Unit })\n\
    1 with2 { s(11) }\n\
    Box { s(12) }\n\
    getB()({ s(13) })\n\
    v { s(14) }\n\
    b(run2 { slot = { s(15) }; slot })\n\
    b(if (f) { lbl@{ s(16) } } else { { s(17) } })\n\
}\n";

/// The receiver row of every receiver lambda's `LocalVariableTable`, in method order, read from the
/// class file: the lambda's own receiver, where its label shows. The rows are compared whole.
fn lambda_receiver_rows(class_file: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(class_file).expect("the class file is written");
    let class = krusty::jvm::classreader::parse_class(&bytes).expect("the class file parses");
    let bodies = krusty::jvm::classreader::ClassBodies::parse(std::sync::Arc::new(bytes))
        .expect("the class file's bodies parse");
    class
        .methods
        .iter()
        .filter(|method| method.name.contains("$lambda$"))
        .flat_map(|method| {
            bodies
                .method_code(&method.name, &method.descriptor)
                .expect("a lambda method has code")
                .locals
        })
        .filter(|local| {
            local.slot == 0 && (local.name.starts_with("$this$") || local.name == "<this>")
        })
        .map(|local| {
            format!(
                "{} {} {} {} {}",
                local.start_pc, local.length, local.slot, local.name, local.descriptor
            )
        })
        .collect()
}

#[test]
fn a_receiver_lambda_takes_the_label_of_the_call_it_is_written_in() {
    let dir = common::scratch_dir().expect("scratch directory");
    let (reference, krusty) = (dir.join("ref"), dir.join("krusty"));
    std::fs::create_dir_all(&reference).expect("reference output directory");
    std::fs::create_dir_all(&krusty).expect("krusty output directory");
    let source = dir.join("LambdaLabels.kt");
    std::fs::write(&source, SHAPES).expect("source file");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let classes =
        common::compile_in_process_metadata_cp(SHAPES, "LambdaLabels", &[common::stdlib_jar()])
            .expect("krusty compiles the shapes");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "LambdaLabelsKt")
        .expect("krusty emits the facade");
    std::fs::write(krusty.join("LambdaLabelsKt.class"), bytes).expect("krusty class file");

    let expected = lambda_receiver_rows(&reference.join("LambdaLabelsKt.class"));
    let receivers = expected
        .iter()
        .map(|row| row.split(' ').nth(3).expect("a local row names its local"))
        .collect::<Vec<_>>();
    assert_eq!(
        receivers,
        [
            "<this>",
            "$this$b",
            "$this$b",
            "$this$b",
            "$this$b",
            "$this$b",
            "$this$b",
            "$this$id",
            "$this$run2",
            "<this>",
            "<this>",
            "$this$with2",
            "$this$Box",
            "$this$invoke",
            "$this$v",
            "<this>",
            "$this$lbl",
            "$this$b",
        ],
        "the reference compiler's receiver names"
    );
    assert_eq!(
        lambda_receiver_rows(&krusty.join("LambdaLabelsKt.class")),
        expected
    );
}
