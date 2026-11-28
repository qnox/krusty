// LANGUAGE: +ContextParameters
// Properties whose accessors take an extension receiver or context parameters, read and written
// from files of the same module that come before and after the declaring file.
// FILE: aEarlierUse.kt
fun earlierUse(): String {
    val box = Counter(1)
    box.twice = 5
    val pair = listOf(1, 2)
    val fromHost = tagWith(Host("h"), "ab")
    scaleWith(Unit10(10), box, 7)
    return "${"abc".lengthPlusOne},${box.twice},${pair.second},$fromHost,${box.scaledBy(10)},${scaledValue(10)}"
}

fun tagWith(host: Host, text: String): String = with(host) { text.tagged }

fun scaleWith(scale: Unit10, box: Counter, next: Int) {
    with(scale) { box.scaled = next }
}

fun Counter.scaledBy(unit: Int): Int = with(Unit10(unit)) { scaled }

fun scaledValue(unit: Int): Int = with(Unit10(unit)) { scaledValue }

fun scaledPrefix(host: Host, unit: Int): String = with(Unit10(unit)) { host.scaledPrefix }

// FILE: declarations.kt
class Counter(var value: Int)

class Unit10(val unit: Int)

val String.lengthPlusOne: Int
    get() = length + 1

var Counter.twice: Int
    get() = value * 2
    set(next) {
        value = next
    }

val <T> List<T>.second: T
    get() = this[1]

context(scale: Unit10)
var Counter.scaled: Int
    get() = value * scale.unit
    set(next) {
        value = next + scale.unit
    }

context(scale: Unit10)
val scaledValue: Int
    get() = scale.unit + 1

open class Host(val prefix: String) {
    open val String.tagged: String
        get() = "$prefix:$this"

    context(scale: Unit10)
    val scaledPrefix: String
        get() = "$prefix${scale.unit}"
}

class LoudHost : Host("L") {
    override val String.tagged: String
        get() = "$prefix!$this"
}

// FILE: main.kt
fun box(): String {
    val earlier = earlierUse()
    if (earlier != "4,34,2,h:ab,170,11") return "earlier: $earlier"

    val box = Counter(3)
    if (box.twice != 6) return "twice: ${box.twice}"
    box.twice += 1
    if (box.value != 7) return "compound: ${box.value}"

    val tagged = tagWith(LoudHost(), "x")
    if (tagged != "L!x") return "member extension: $tagged"

    scaleWith(Unit10(2), box, 1)
    val later = "${box.scaledBy(2)},${scaledValue(2)},${scaledPrefix(Host("p"), 2)}"
    if (later != "6,3,p2") return "context: $later"
    if ("".lengthPlusOne != 1 || listOf("a", "b").second != "b") return "extension"
    return "OK"
}
