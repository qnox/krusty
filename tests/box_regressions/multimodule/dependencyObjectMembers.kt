// MODULE: lib
object Counter {
    var count = 0

    fun next(): Int {
        count++
        return count
    }

    fun greet(prefix: String): String = prefix + "K"
}

// MODULE: main(lib)
fun box(): String {
    if (Counter.next() != 1) return "fail next"
    if (Counter.next() != 2) return "fail second next"
    if (Counter.count != 2) return "fail count"
    return Counter.greet("O")
}
