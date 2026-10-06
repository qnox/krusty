//! A one-word safe call leaves its receiver on the stack: `dup`, `ifnull` to `pop`, and the
//! non-null path consumes that value. kotlinc does not store the receiver in a temporary.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const SRC: &str = "\
class Host {\n\
    fun close() {}\n\
    fun take(x: Int) {}\n\
    val n: Int = 1\n\
    fun read(): Int = n\n\
}\n\
class Prefix(val text: String)\n\
fun Host.extension(x: Int) { take(x) }\n\
context(prefix: Prefix)\n\
fun Host.contextExtension(x: Int) { take(prefix.text.length + x) }\n\
@JvmInline value class Token(val text: String)\n\
fun <T> identity(value: T): T = value\n\
fun closeReceiver(host: Host?) { host?.close() }\n\
fun closeCall() { make()?.close() }\n\
fun make(): Host? = null\n\
fun discardRead(host: Host?) { host?.read() }\n\
fun withArg(host: Host?, x: Int) { host?.take(x) }\n\
fun withExtension(host: Host?, x: Int) { host?.extension(x) }\n\
context(prefix: Prefix)\n\
fun withContextExtension(host: Host?, x: Int) { host?.contextExtension(x) }\n\
fun readProp(host: Host?): Int? = host?.n\n\
fun genericValue(token: Token?): String? = identity(token)?.text\n\
";

#[test]
fn a_local_safe_call_duplicates_its_receiver() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void closeReceiver(",
    );
}

#[test]
fn a_call_safe_call_duplicates_its_receiver() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void closeCall(",
    );
}

#[test]
fn a_discarded_safe_call_result_is_popped() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void discardRead(",
    );
}

#[test]
fn a_safe_call_argument_follows_the_duplicated_receiver() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void withArg(",
    );
}

#[test]
fn a_safe_property_read_duplicates_its_receiver() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final java.lang.Integer readProp(",
    );
}

#[test]
fn a_safe_extension_call_duplicates_its_checked_receiver_operand() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void withExtension(",
    );
}

#[test]
fn a_context_parameter_does_not_replace_the_extension_receiver_operand() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final void withContextExtension(",
    );
}

#[test]
fn an_erased_generic_value_class_receiver_crosses_the_temporary_representation_boundary() {
    expect_method_matches(
        "SafeCallDup",
        SRC,
        "SafeCallDupKt",
        "public static final java.lang.String genericValue-",
    );
}
