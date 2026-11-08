
private fun add(left: Int, right: Int): Int {
    val holder = object { fun add(left: Int, right: Int) = left + right }
    return holder.add(left, right)
}
fun box(): String = if (add(4, 3) == 7) "OK" else "shadowed"
