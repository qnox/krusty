//! `for` loops retain kotlinc's subject and local-slot strategy.
//!
//! Repository-owned protocol declarations keep this regression on the generic dispatch, extension,
//! and member-extension paths rather than a stdlib classifier that may acquire specialized lowering.
//! Indexed array and `CharSequence` loops re-read a receiver, parameter, or stable captured `val`;
//! only a subject that can change during iteration is copied to a temporary.

use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = r#"
var trace = 0

fun mark(digit: Int) {
    trace = trace * 10 + digit
}

class Cursor(private val end: Int) {
    private var current = 0

    operator fun hasNext(): Boolean = current < end

    operator fun next(): Int {
        val value = current
        current++
        return value
    }
}

class DispatchRange(private val end: Int) {
    operator fun iterator(): Cursor {
        mark(2)
        return Cursor(end)
    }
}

fun dispatchSubject(): DispatchRange {
    mark(1)
    return DispatchRange(3)
}

fun dispatchLoop(): Int {
    var sum = 0
    for (value in dispatchSubject()) sum += value
    return sum
}

class ExtensionRange(val end: Int)

fun extensionSubject(): ExtensionRange {
    mark(1)
    return ExtensionRange(3)
}

operator fun ExtensionRange.iterator(): Cursor {
    mark(2)
    return Cursor(end)
}

fun extensionLoop(): Int {
    var sum = 0
    for (value in extensionSubject()) sum += value
    return sum
}

class MemberRange(val end: Int)

fun memberSubject(): MemberRange {
    mark(1)
    return MemberRange(3)
}

class RangeScope {
    operator fun MemberRange.iterator(): Cursor {
        mark(2)
        return Cursor(end)
    }

    fun memberExtensionLoop(): Int {
        var sum = 0
        for (value in memberSubject()) sum += value
        return sum
    }
}

fun box(): String {
    trace = 0
    if (dispatchLoop() != 3 || trace != 12) return "FAIL dispatch: $trace"
    trace = 0
    if (extensionLoop() != 3 || trace != 12) return "FAIL extension: $trace"
    trace = 0
    if (RangeScope().memberExtensionLoop() != 3 || trace != 12) {
        return "FAIL member extension: $trace"
    }
    return "OK"
}
"#;

fn assert_methods_match(built: &common::ReferenceComparison, methods: &[&str]) {
    for method in methods {
        let reference = method_instructions(&built.reference, method);
        assert!(
            !reference.is_empty(),
            "{method} not found in kotlinc output"
        );
        assert_eq!(
            method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn iterator_protocol_receivers_take_kotlincs_slots() {
    let classpath = [common::stdlib_jar()];
    let Some(top_level) = compare_with_kotlinc_plugin(
        "IteratorProtocolSlots",
        SOURCE,
        "IteratorProtocolSlotsKt",
        &classpath,
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_methods_match(&top_level, &["int dispatchLoop(", "int extensionLoop("]);

    let Some(member_extension) = compare_with_kotlinc_plugin(
        "IteratorProtocolSlots",
        SOURCE,
        "RangeScope",
        &classpath,
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_methods_match(&member_extension, &["int memberExtensionLoop("]);
}

#[test]
fn iterator_protocol_subject_is_evaluated_once_before_iterator() {
    common::expect_box_same_as_kotlinc(SOURCE, "IteratorProtocolOrder");
}

const RECEIVERS: &str = r#"fun Array<String>.joined(): String {
    var result = ""
    for (element in this) result += element
    return result
}

fun String.spaced(): String {
    var result = ""
    for (c in this) result += "$c "
    return result
}

class Letters(private val letters: CharArray) {
    private fun CharArray.count(): Int {
        var n = 0
        for (c in this) n++
        return n
    }

    fun total(): Int = letters.count()
}

fun box(): String {
    val joined = arrayOf("O", "K").joined()
    val spaced = "ab".spaced()
    val total = Letters(charArrayOf('x', 'y')).total()
    return if (spaced == "a b " && total == 2) joined else "fail"
}
"#;

#[test]
fn an_indexed_loop_over_a_receiver_reads_the_receiver() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "IndexedLoopReceiver",
        RECEIVERS,
        &["IndexedLoopReceiverKt", "Letters"],
    );
}

/// A class `this` is the dispatch receiver itself. `ForLoopsLowering` therefore re-reads its
/// parameter slot for each indexed `CharSequence` access rather than copying it to a loop subject.
const DISPATCH_RECEIVER: &str = r#"class DispatchText(private val text: String) : CharSequence {
    override val length: Int get() = text.length
    override fun get(index: Int): Char = text[index]
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =
        text.subSequence(startIndex, endIndex)

    fun joined(): String {
        var result = ""
        for (c in this) result += c
        return result
    }
}

fun box(): String = DispatchText("OK").joined()
"#;

#[test]
fn an_indexed_loop_over_the_dispatch_receiver_reads_its_parameter_slot() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "IndexedLoopDispatchReceiver",
        DISPATCH_RECEIVER,
        &["IndexedLoopDispatchReceiverKt", "DispatchText"],
    );
}

/// A local function receives the enclosing extension `this` as an implicit-receiver capture. Its
/// loop must re-read that capture slot just as the extension body re-reads its receiver slot.
const CAPTURED_IMPLICIT_RECEIVER: &str = r#"fun String.joined(): String {
    fun local(): String {
        var result = ""
        for (c in this@joined) result += c
        return result
    }
    return local()
}

fun box(): String = "OK".joined()
"#;

#[test]
fn an_indexed_loop_reads_a_captured_implicit_receiver() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "IndexedLoopCapturedReceiver",
        CAPTURED_IMPLICIT_RECEIVER,
        &["IndexedLoopCapturedReceiverKt"],
    );
}

const CAPTURED: &str = r#"fun inlined(values: IntArray): String = buildString {
    for (value in values) append(value)
}

fun nested(text: String): String {
    val copy = text
    fun joined(): String = buildString { for (c in copy) append(c) }
    return joined()
}

fun deferred(values: IntArray): () -> Int = {
    var sum = 0
    for (value in values) sum += value
    sum
}

fun reassigned(text: String): String {
    var current = text
    val result = buildString { for (c in current) append(c) }
    current = ""
    return result + current
}

fun box(): String =
    if (inlined(intArrayOf(1, 2)) == "12" && nested("ab") == "ab" && deferred(intArrayOf(1, 2))() == 3 &&
        reassigned("c") == "c") "OK"
    else "fail"
"#;

#[test]
fn an_indexed_loop_over_a_captured_value_reads_it_unless_it_is_reassigned() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "IndexedLoopCaptured",
        CAPTURED,
        &["IndexedLoopCapturedKt"],
    );
}

/// A mutable binding stays mutable while it is forwarded through several local-function capture
/// layers. The innermost indexed loop must keep the subject copy that protects one iteration from
/// a later write through the shared cell.
const DEEPLY_CAPTURED_MUTABLE: &str = r#"fun joinedDeep(values: IntArray): String {
    var current = values
    fun first(): String {
        fun second(): String {
            fun third(): String {
                var result = ""
                for (value in current) result += value
                return result
            }
            return third()
        }
        return second()
    }
    val result = first()
    current = intArrayOf()
    return result + current.size
}

fun box(): String = if (joinedDeep(intArrayOf(1, 2)) == "120") "OK" else "fail"
"#;

#[test]
fn a_deeply_captured_mutable_subject_keeps_its_loop_copy() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "IndexedLoopDeepMutable",
        DEEPLY_CAPTURED_MUTABLE,
        &["IndexedLoopDeepMutableKt"],
    );
}
