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

fun make(value: Int): Marker = Hidden(value)

fun box(): String {
    val direct = through(7)
    val returned = make(4).mark()
    return if (direct == 7 && returned == 4) "OK" else "wrong"
}
