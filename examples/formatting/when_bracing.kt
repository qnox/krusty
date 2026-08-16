fun describe(x: Int): String {
    return when (x) {
        1 -> "one"
        2 ->
            "two"
        else -> "other"
    }
}
fun log(x: Int) {
    when (x) {
        1 ->
            println("one")
        else -> println("other")
    }
}
