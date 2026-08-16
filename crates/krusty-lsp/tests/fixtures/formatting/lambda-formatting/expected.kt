val b = { 1 }
val c = { x: Int -> x + 1 }
val d = {
    1
}

fun f() {
    listOf(1).map { it + 1 }.filter { it > 0 }
}
