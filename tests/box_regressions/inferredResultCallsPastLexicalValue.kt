// A bare call `value(x)` whose nearest `value` is a lexical value without an applicable `invoke`
// moves on to the function of that name, in an inferred result as in a body.
fun value(x: Int) = x + 1

fun shadowedByParameter(value: Any?) = value(1)

fun shadowedByLocal(): Int {
    val value = "text"
    return value(2)
}

// A function value whose parameters take the arguments is the nearest level and wins.
fun applicableValue(value: (Int) -> Int) = value(4)

class Processor {
    private var placeholders = ""
    private var nulls = 0

    fun value(placeholder: String, value: Any?) = apply {
        placeholders += placeholder
        if (value == null) nulls++
    }

    fun optionalValue(placeholder: String, value: Any?) = value(placeholder, value)

    fun summary() = placeholders + nulls
}

fun box(): String {
    if (shadowedByParameter(null) != 2) return "fail parameter"
    if (shadowedByLocal() != 3) return "fail local"
    if (applicableValue { it * 10 } != 40) return "fail applicable value"
    val processor = Processor().optionalValue("a", 1).optionalValue("b", null)
    if (processor.summary() != "ab1") return "fail member"
    return "OK"
}
