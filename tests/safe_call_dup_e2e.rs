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
fun closeReceiver(host: Host?) { host?.close() }\n\
fun closeCall() { make()?.close() }\n\
fun make(): Host? = null\n\
fun discardRead(host: Host?) { host?.read() }\n\
fun withArg(host: Host?, x: Int) { host?.take(x) }\n\
fun readProp(host: Host?): Int? = host?.n\n\
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
