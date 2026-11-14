package fixture

interface Marker {
    fun mark(): Int
}

private class Hidden(value: Int) : Marker {
    val first: Int = value
    val second: Int = first
    override fun mark(): Int = second
    fun member(): Int = first
}

fun make(value: Int): Int {
    val hidden = Hidden(value)
    return hidden.member()
}

fun marked(value: Int): Int {
    val hidden = Hidden(value)
    return hidden.mark()
}

fun asMarker(value: Int): Marker {
    return Hidden(value)
}
