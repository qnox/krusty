package fixture

interface Marker {
    fun mark(): Int
}

private class Hidden(value: Int) : Marker {
    val stored: Int = value
    override fun mark(): Int = stored
}

fun through(value: Int): Int {
    val marker: Marker = Hidden(value)
    return marker.mark()
}
