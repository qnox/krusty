//! A loop that reads its elements by index re-reads a subject that cannot change.
//!
//! kotlinc's `ForLoopsLowering` copies the iterable to a temporary only when it can change while
//! the loop runs (`canChangeValueDuringExecution`). A receiver, a parameter or a `val` cannot, also
//! when a lambda captured it, so an array or `String` loop over one declares its index and length
//! and reads the subject again for each element.

use super::common;

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
