// LANGUAGE: +CompanionBlocks
// A compound assignment to a property of a `companion { … }` block, named through its class, reads
// and writes the block's storage: the qualifier is no value to evaluate.

var log = ""

class Counter {
    companion {
        var count: Int = 1
        var label: String = "O"
        var order: Int = 0
    }
}

fun step(): Int {
    log += "s"
    return 2
}

fun box(): String {
    Counter.count += 4
    Counter.count *= step()
    if (Counter.count != 10) return "FAIL count ${Counter.count}"
    Counter.label += "K"
    if (Counter.label != "OK") return "FAIL label ${Counter.label}"
    Counter.order -= 1
    Counter.order++
    if (Counter.order != 0) return "FAIL order ${Counter.order}"
    if (log != "s") return "FAIL log $log"
    return Counter.label
}
