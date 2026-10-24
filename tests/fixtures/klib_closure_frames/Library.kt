package fixture
fun make(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return { a, b ->
        val nested = { seed: Int -> "$saved:$prefix:$a:$b:$seed" }
        nested(9)
    }
}
inline fun inlineMake(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return { a, b ->
        val nested = { seed: Int -> "$saved:$prefix:$a:$b:$seed" }
        nested(9)
    }
}
fun anonymousMake(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return fun(a: Int, b: Int): String {
        val nested = fun(seed: Int): String { return "$saved:$prefix:$a:$b:$seed" }
        return nested(9)
    }
}

fun unsupportedComparator(): Comparator<Int> = Comparator { a, b -> a - b }
