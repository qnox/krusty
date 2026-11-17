// A function value printed through a generic parameter, as a default argument's lambda is: every
// backend prints its class's name, which this does not pin, as each names the class its own way.

fun <T> show(value: T): String = "$value"

fun <T> rendered(value: T, render: (T) -> String = { "$it" }): String = render(value)

class Counter(var count: Int) {
    fun next(): Int = ++count
}

fun box(): String {
    val step = 2
    var total = 0
    val strings = arrayOf(
        show { 1 },
        show { x: Int -> x + step },
        show { total += step },
        show(Counter(0)::next),
        show(Counter::next),
        rendered({ step }),
        rendered(Counter(1)::next),
    )
    for (string in strings) {
        if (string.isEmpty() || string == "null") return "Fail: $string"
    }
    return "OK"
}
