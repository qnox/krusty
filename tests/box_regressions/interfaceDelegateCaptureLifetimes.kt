interface Reader { fun read(): String }

private fun reader(value: String): Reader = object : Reader {
    override fun read(): String = value
}

private fun duringConstruction(impl: Reader) = object {
    val delegated = object : Reader by impl {}
    fun read(): String = delegated.read()
}

private fun afterConstruction(impl: Reader) = object {
    fun delegated() = object : Reader by impl {}
}

fun box(): String {
    val first = reader("O")
    val second = reader("K")
    val eagerFirst = duringConstruction(first)
    val eagerSecond = duringConstruction(second)
    if (eagerFirst.read() != "O" || eagerSecond.read() != "K") return "constructor"
    val laterFirst = afterConstruction(first)
    val laterSecond = afterConstruction(second)
    if (laterFirst.delegated().read() != "O") return "first"
    if (laterSecond.delegated().read() != "K") return "second"
    if (laterFirst.delegated().read() != "O") return "retained"
    return "OK"
}
