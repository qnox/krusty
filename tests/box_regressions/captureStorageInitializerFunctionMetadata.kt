
private fun copy(read: String.() -> Int) = object { val result = "word".read() }
fun box(): String = if (copy { length }.result == 4) "OK" else "function"
