// LANGUAGE: +CompanionBlocksAndExtensions +CompanionBlocks +CompanionExtensions
// A companion extension property is associated with its classifier, not called on a receiver
// value: files before and after the declaring one read and write it with no receiver operand,
// beside an ordinary extension property of the same classifier.
// FILE: aEarlierUse.kt
fun earlierUse(): String {
    Counter.total = 4
    Counter.total += 1
    return "${Counter.total},${Counter.label},${Counter(2).doubled}"
}

// FILE: declarations.kt
private var stored = 0

class Counter(val value: Int)

companion var Counter.total: Int
    get() = stored
    set(next) {
        stored = next
    }

companion val Counter.label: String
    get() = "counter$stored"

val Counter.doubled: Int
    get() = value * 2

// FILE: main.kt
fun box(): String {
    val earlier = earlierUse()
    if (earlier != "5,counter5,4") return "earlier: $earlier"
    Counter.total *= 3
    if (Counter.total != 15) return "total: ${Counter.total}"
    if (Counter.label != "counter15") return "label: ${Counter.label}"
    if (Counter(5).doubled != 10) return "doubled"
    return "OK"
}
