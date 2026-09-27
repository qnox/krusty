//! A same-file extension call passes its receiver and arguments straight to the call, in source
//! order, as kotlinc does: no copy of either to a local first.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const SRC: &str = "class Api(val tag: String)\n\
fun Api.label(n: Int): String = tag + n\n\
fun Api.labelled(n: Int, suffix: String = \"!\"): String = tag + n + suffix\n\
var calls = 0\n\
fun api(): Api {\n\
    calls += 1\n\
    return Api(\"a\")\n\
}\n\
fun count(): Int {\n\
    calls += 10\n\
    return calls\n\
}\n\
fun fromParameter(a: Api): String = a.label(1)\n\
fun fromCalls(): String = api().label(count())\n\
fun withDefault(): String = api().labelled(count())\n\
class Scope(val name: String) {\n\
    fun Api.scoped(n: Int): String = name + tag + n\n\
    fun go(a: Api): String = a.scoped(count())\n\
}\n\
fun box(): String {\n\
    val r = fromCalls() + withDefault() + Scope(\"s\").go(api())\n\
    return if (r == \"a11a22!sa33\" && calls == 33) \"OK\" else \"F:$r:$calls\"\n\
}\n";

#[test]
fn an_extension_call_on_a_parameter_matches_kotlinc() {
    expect_method_matches(
        "ExtensionOperands",
        SRC,
        "ExtensionOperandsKt",
        "public static final java.lang.String fromParameter(",
    );
}

#[test]
fn an_extension_call_on_calls_matches_kotlinc() {
    expect_method_matches(
        "ExtensionOperands",
        SRC,
        "ExtensionOperandsKt",
        "public static final java.lang.String fromCalls(",
    );
}

#[test]
fn a_defaulted_extension_call_matches_kotlinc() {
    expect_method_matches(
        "ExtensionOperands",
        SRC,
        "ExtensionOperandsKt",
        "public static final java.lang.String withDefault(",
    );
}

#[test]
fn a_member_extension_call_matches_kotlinc() {
    expect_method_matches(
        "ExtensionOperands",
        SRC,
        "Scope",
        "public final java.lang.String go(",
    );
}

#[test]
fn extension_call_operands_evaluate_once_in_source_order() {
    common::expect_box_ok_with_stdlib(SRC, "ExtensionOperandsRun");
}

// A nullable-receiver extension called on a scalar value boxes the receiver where it is passed.
const NULLABLE_RECEIVER: &str =
    "fun Int?.orZero(step: Int): Int = if (this == null) 0 else this + step\n\
fun count(): Int = 2\n\
fun boxed(n: Int): Int = n.orZero(count())\n\
fun box(): String = if (boxed(10) == 12) \"OK\" else \"F:${boxed(10)}\"\n";

#[test]
fn a_nullable_receiver_extension_on_a_scalar_matches_kotlinc() {
    expect_method_matches(
        "NullableReceiver",
        NULLABLE_RECEIVER,
        "NullableReceiverKt",
        "public static final int boxed(",
    );
}

#[test]
fn a_nullable_receiver_extension_on_a_scalar_runs() {
    common::expect_box_ok_with_stdlib(NULLABLE_RECEIVER, "NullableReceiverRun");
}

// A call with a context parameter takes the context argument first and the extension receiver
// after it. The context argument is an implicit value, so kotlinc passes all three operands directly
// too, and each is still evaluated once, in source order.
const CONTEXT: &str = "class Api(val tag: String)\n\
var log = \"\"\n\
fun api(): Api {\n\
    log += \"r\"\n\
    return Api(\"a\")\n\
}\n\
fun count(): Int {\n\
    log += \"v\"\n\
    return 1\n\
}\n\
fun scope(): String {\n\
    log += \"c\"\n\
    return \"x\"\n\
}\n\
context(c: String) fun Api.contextual(n: Int): String = c + tag + n\n\
context(c: String) fun inContext(): String = api().contextual(count())\n\
fun String.drive(): String = inContext()\n\
fun box(): String {\n\
    val r = scope().drive()\n\
    return if (r == \"xa1\" && log == \"crv\") \"OK\" else \"F:$r:$log\"\n\
}\n";

#[test]
fn a_context_extension_call_matches_kotlinc() {
    expect_method_matches(
        "ContextOperands",
        CONTEXT,
        "ContextOperandsKt",
        "public static final java.lang.String inContext(",
    );
}

#[test]
fn a_context_supplied_by_a_receiver_matches_kotlinc() {
    expect_method_matches(
        "ContextOperands",
        CONTEXT,
        "ContextOperandsKt",
        "public static final java.lang.String drive(",
    );
}

#[test]
fn context_extension_call_operands_evaluate_once_in_source_order() {
    common::expect_box_ok_with_stdlib(CONTEXT, "ContextOperandsRun");
}
