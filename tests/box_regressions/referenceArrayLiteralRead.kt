var events = ""

fun element(value: String): String {
    events += value
    return value
}

fun receiver(value: Array<String>): Array<out Any?> {
    events += "R"
    return value
}

fun index(): Int {
    events += "I"
    return 1
}

fun box(): String {
    val values = arrayOf(element("O"), element("K"))
    val projected: Array<out Any?> = values
    if (projected !== values) return "array identity"
    if (receiver(values)[index()] != "K") return "projected read"
    if (events != "OKRI") return "evaluation order: $events"
    val nullable = arrayOf<String?>(null, "OK")
    if (nullable[0] != null) return "null element"
    return nullable[1] ?: "nullable read"
}
