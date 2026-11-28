class Marker

fun classify(x: Any): Int = when (x) {
    is String -> 1
    is Marker -> 2
    is Any -> 3
}

fun nullable(x: Any?): Int = when (x) {
    is String -> 1
    null -> 2
    is Any -> 3
}

fun box(): String {
    if (classify("a") != 1) return "String"
    if (classify(Marker()) != 2) return "Marker"
    if (classify(5) != 3) return "Any"
    if (nullable(null) != 2) return "null"
    if (nullable(5) != 3) return "nullable Any"
    return "OK"
}
