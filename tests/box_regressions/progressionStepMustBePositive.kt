// WITH_STDLIB

fun failure(block: () -> Unit): String? =
    try {
        block()
        null
    } catch (e: IllegalArgumentException) {
        e.message
    }

fun box(): String {
    val step = 0
    var total = 0
    val intStep = failure { for (i in 1..5 step step) total += i }
    if (intStep != "Step must be positive, was: 0.") return "FAIL int step: $intStep"

    val longStep = 0L - 3L
    val long = failure { for (i in 10L downTo 1L step longStep) total += i.toInt() }
    if (long != "Step must be positive, was: -3.") return "FAIL long step: $long"

    if (total != 0) return "FAIL a rejected step ran the loop: $total"
    return "OK"
}
