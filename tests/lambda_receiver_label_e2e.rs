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
