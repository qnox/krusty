package fixture

interface Sized {
    val size: Int
}

private class Hidden(value: Int) : Sized {
    override val size: Int = value
}

fun through(value: Int): Int {
    val sized: Sized = Hidden(value)
    return sized.size
}

fun make(value: Int): Sized = Hidden(value)

fun box(): String {
    val direct = through(7)
    val returned = make(4).size
    return if (direct == 7 && returned == 4) "OK" else "wrong"
}
