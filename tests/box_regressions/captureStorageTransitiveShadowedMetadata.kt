
private fun copy(): Int {
    val value = 7
    fun read() = value
    val holder = object { fun get(value: Int) = read() + value }
    return holder.get(1)
}
fun box(): String = if (copy() == 8) "OK" else "transitive"
