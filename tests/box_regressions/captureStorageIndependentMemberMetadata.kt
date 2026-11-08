
private fun copy(value: Int) = object {
    fun original() = value
    fun argument(value: Int) = value
}
fun box(): String {
    val holder = copy(7)
    return if (holder.original() == 7 && holder.argument(2) == 2) "OK" else "scope"
}
