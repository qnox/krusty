// `Unit` is comparable to a subject of its supertype `Any?`, as in a reply dispatcher that
// treats a block's `Unit` result as "no reply".

fun describe(block: () -> Any?): String =
    when (val result = block()) {
        Unit -> "unit"
        is Int -> "int$result"
        else -> "other$result"
    }

fun box(): String {
    if (describe { Unit } != "unit") return "unit"
    if (describe { 7 } != "int7") return "int"
    if (describe { "s" } != "others") return "other"
    return "OK"
}
