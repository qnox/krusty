// A `Unit` condition compares with the subject by equality, like any other object.
fun describe(value: Any?): String = when (value) {
    Unit -> "unit"
    is String -> "string"
    else -> "other"
}

fun run(block: () -> Any?): String = when (val result = block()) {
    Unit -> "none"
    else -> result.toString()
}

fun box(): String {
    if (describe(Unit) != "unit") return "Unit subject: ${describe(Unit)}"
    if (describe("s") != "string") return "String subject: ${describe("s")}"
    if (describe(null) != "other") return "null subject: ${describe(null)}"
    if (describe(1) != "other") return "Int subject: ${describe(1)}"
    if (run { } != "none") return "Unit result: ${run { }}"
    if (run { 7 } != "7") return "Int result: ${run { 7 }}"
    return "OK"
}
