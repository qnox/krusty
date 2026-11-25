// WITH_STDLIB

class Counter {
    var runs = 0
}

fun box(): String {
    val counter = Counter()
    val computed = lazy {
        counter.runs++
        "v" + counter.runs
    }
    if (computed.isInitialized()) return "initialized before the first read"
    if (counter.runs != 0) return "ran before the first read"
    val first = computed.value
    if (first != "v1" || computed.value != first || counter.runs != 1) {
        return "ran other than once: $first ${counter.runs}"
    }
    if (!computed.isInitialized()) return "not initialized after a read"
    if (computed.toString() != "v1") return "rendered as $computed"

    var attempts = 0
    val flaky = lazy {
        attempts++
        if (attempts == 1) throw IllegalStateException("first")
        "second"
    }
    try {
        flaky.value
        return "no exception from the first read"
    } catch (_: IllegalStateException) {
    }
    if (flaky.isInitialized()) return "initialized after an initializer threw"
    if (flaky.value != "second" || attempts != 2) return "retried $attempts times"

    return "OK"
}
