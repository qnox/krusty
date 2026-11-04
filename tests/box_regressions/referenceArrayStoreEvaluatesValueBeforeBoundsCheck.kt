class Tag(val text: String)

var trace = ""

fun slots(): Array<Tag> {
    trace += "a"
    return arrayOf(Tag("0"))
}

fun at(index: Int): Int {
    trace += "i"
    return index
}

fun stored(text: String): Tag {
    trace += "v"
    return Tag(text)
}

fun box(): String {
    val array = slots()
    array[at(0)] = stored("x")
    if (array[0].text != "x" || trace != "aiv") return "in range: $trace"

    trace = ""
    try {
        slots()[at(1)] = stored("y")
        return "out of range stored"
    } catch (expected: Throwable) {
        if (expected !is IndexOutOfBoundsException) return "out of range threw another exception"
        if (trace != "aiv") return "out of range: $trace"
    }

    trace = ""
    val failure = Throwable("value")
    try {
        slots()[at(-1)] = run<Tag> {
            trace += "t"
            throw failure
        }
        return "throwing value stored"
    } catch (thrown: Throwable) {
        if (thrown is IndexOutOfBoundsException) return "bounds check before value"
        if (thrown !== failure || trace != "ait") return "throwing value: $trace"
    }
    return "OK"
}
