//! A one-word safe call leaves its receiver on the stack: `dup`, `ifnull` to `pop`, and the
//! non-null path consumes that value. kotlinc does not store the receiver in a temporary.

use super::common;

const SRC: &str = "\
class Host {\n\
    fun close() {}\n\
    fun take(x: Int) {}\n\
    var n: Int = 1\n\
    var next: Host? = null\n\
    fun read(): Int = n\n\
}\n\
class Prefix(val text: String)\n\
fun Host.extension(x: Int) { take(x) }\n\
context(prefix: Prefix)\n\
fun Host.contextExtension(x: Int) { take(prefix.text.length + x) }\n\
@JvmInline value class Token(val text: String)\n\
fun <T> identity(value: T): T = value\n\
fun Int.bump(): Int = this + 1\n\
fun Long.bumpWide(): Long = this + 1L\n\
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
fun nestedArgument(host: Host?, flag: Boolean) {\n\
    host?.take(if (flag) { val argument = 1; argument } else 0)\n\
}\n\
fun chain(host: Host?): Int? = host?.next?.n\n\
fun scalar(value: Int?): Int? = value?.bump()\n\
fun wide(value: Long?): Long? = value?.bumpWide()\n\
";

#[test]
fn safe_call_duplication_and_its_refusals_match_kotlinc() {
    let methods = [
        // A local safe call duplicates its receiver.
        "public static final void closeReceiver(",
        // A call result is evaluated once and duplicated in place.
        "public static final void closeCall(",
        // A discarded selector result is popped inside the guard.
        "public static final void discardRead(",
        // Ordinary arguments follow the receiver already on the stack.
        "public static final void withArg(",
        // A property read consumes the duplicated receiver.
        "public static final java.lang.Integer readProp(",
        // A selected extension consumes its recorded extension-receiver operand.
        "public static final void withExtension(",
        // A context parameter precedes, but does not replace, the recorded extension receiver.
        "public static final void withContextExtension(",
        // An erased value crosses the eliminated temporary's representation boundary.
        "public static final java.lang.String genericValue-",
        // Nested selector blocks retain their ordinary lexical/debug lifecycle.
        "public static final void nestedArgument(",
        // A shared safe-call chain keeps its receiver temporaries.
        "public static final java.lang.Integer chain(",
        // Nullable scalar and wide receivers keep their representation temporaries.
        "public static final java.lang.Integer scalar(",
        "public static final java.lang.Long wide(",
    ];
    let results = common::class_bytes_diffs_against_kotlinc(
        "SafeCallDup",
        &[],
        SRC,
        "SafeCallDupKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    for (method, result) in methods.into_iter().zip(results) {
        result.unwrap_or_else(|difference| panic!("{method}: {difference}"));
    }
}

/// An inlined `let` whose lambda branches gives the method an `int` jump and constant, so the
/// constant-condition pass runs over it. The label in front of the safe call's null path still
/// stands beside the reached jump target, and the receiver stays on the stack as kotlinc keeps it.
const BRANCHING_LET: &str = "\
class Options(val config: Set<String>)\n\
fun picked(name: String, options: Options?): String? =\n\
    options?.let {\n\
        when {\n\
            name in it.config -> \"config\"\n\
            else -> null\n\
        }\n\
    }\n\
fun inline(options: Options?): String? = options?.let { if (it.config.isEmpty()) null else \"x\" }\n\
fun stored(options: Options?): String? {\n\
    val result = options?.let { if (it.config.isEmpty()) null else \"x\" }\n\
    return result\n\
}\n\
";

#[test]
fn a_safe_call_let_whose_lambda_branches_keeps_its_receiver_on_the_stack() {
    common::byte_diff_against_kotlinc_cp(
        "SafeCallBranchingLet",
        BRANCHING_LET,
        "SafeCallBranchingLetKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("SafeCallBranchingLetKt differs from kotlinc: {diff}"));
}
