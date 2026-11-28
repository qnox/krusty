fun describe(value: Any?): String =
    when (value) {
        is Unit -> "unit"
        is String -> "string"
        null -> "null"
        else -> "other"
    }

fun box(): String {
    val unit: Any? = Unit
    if (unit !is Unit) return "FAIL: Unit is not a Unit"
    val text: Any? = "text"
    if (text is Unit) return "FAIL: a String is a Unit"
    val nothing: Any? = null
    if (nothing is Unit) return "FAIL: null is a Unit"
    if ((nothing as? Unit) != null) return "FAIL: null cast to Unit"
    if ((unit as? Unit) == null) return "FAIL: Unit safe-cast"
    val shape = listOf(Unit, "s", null, 1).map(::describe)
    if (shape != listOf("unit", "string", "null", "other")) return "FAIL: $shape"
    return "OK"
}
