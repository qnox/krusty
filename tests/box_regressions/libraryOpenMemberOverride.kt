import kotlin.random.Random

class Fixed(private val value: Int) : Random() {
    override fun nextBits(bitCount: Int): Int = value
    override fun nextInt(): Int = value + 1
}

fun box(): String {
    val r: Random = Fixed(7)
    if (r.nextInt() != 8) return "FAIL nextInt ${r.nextInt()}"
    if (r.nextBits(3) != 7) return "FAIL nextBits"
    return "OK"
}
