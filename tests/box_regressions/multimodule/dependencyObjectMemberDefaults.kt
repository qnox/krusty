// MODULE: lib
object Counter {
    var count = 0

    fun next(): Int {
        count++
        return count
    }
}

// Each omitted default calls the library object's member, in declaration order, after the
// supplied arguments.
fun labelled(prefix: String, first: Int = Counter.next(), second: Int = Counter.next() * 10): String =
    prefix + first + ":" + second

// MODULE: main(lib)
fun local(value: Int = Counter.next()): Int = value

fun box(): String {
    if (labelled("a") != "a1:20") return "fail both defaults"
    if (labelled("b", 7) != "b7:30") return "fail second default"
    if (labelled("c", 8, 9) != "c8:9") return "fail no defaults"
    if (local() != 4) return "fail local default"
    if (Counter.count != 4) return "fail count"
    return "OK"
}
