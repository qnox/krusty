private class CaptureMark(val value: Int)

private fun sharedFactory(): () -> (() -> String) {
    var integer = 0
    var wide = 0L
    var single = 0.0f
    var double = 0.0
    var text: String? = null
    var reference = CaptureMark(0)
    val advance: () -> Unit = {
        integer++
        wide += 2L
        single += 0.5f
        double += 0.25
        text = (text ?: "") + "x"
        reference = CaptureMark(integer)
    }
    return {
        val step = advance
        val run: () -> String = {
            step()
            val consistent = when (integer) {
                1 -> wide == 2L && single == 0.5f && double == 0.25 && text == "x"
                2 -> wide == 4L && single == 1.0f && double == 0.5 && text == "xx"
                3 -> wide == 6L && single == 1.5f && double == 0.75 && text == "xxx"
                4 -> wide == 8L && single == 2.0f && double == 1.0 && text == "xxxx"
                else -> false
            }
            if (consistent && reference.value == integer) "OK" else "shared state"
        }
        run
    }
}

fun box(): String {
    val factory = sharedFactory()
    val first = factory()
    val second = factory()
    if (first() != "OK") return "first invocation"
    if (second() != "OK") return "second closure"
    val alias = first
    if (alias() != "OK") return "aliased invocation"
    if (second() != "OK") return "repeated invocation"
    return "OK"
}
