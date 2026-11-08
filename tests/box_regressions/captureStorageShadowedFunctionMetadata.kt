
private fun copy(read: String.() -> Int) = object {
    fun run(read: String.() -> Int) = "word".read()
}
fun box(): String = if (copy { length }.run { length + 1 } == 5) "OK" else "function scope"
