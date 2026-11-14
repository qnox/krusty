data class Ints(val xs: IntArray) {
    override fun toString(): String = "Ints"
}
data class MaybeInts(val xs: IntArray?) {
    override fun toString(): String = "MaybeInts"
}
data class Texts(val xs: Array<String?>) {
    override fun toString(): String = "Texts"
}
data class Inner(val n: Int)
data class Nested(val xs: Array<Inner>) {
    override fun toString(): String = "Nested"
}
data class Mixed(val label: String, val xs: IntArray, val count: Int) {
    override fun toString(): String = "Mixed"
}

fun box(): String {
    val shared = intArrayOf(1, -2)
    val ints = Ints(shared)
    val before = ints.hashCode()
    shared[0] = 7
    val text = "$before;${ints.hashCode()};${Ints(intArrayOf()).hashCode()};" +
        "${MaybeInts(intArrayOf(3)).hashCode()};${MaybeInts(null).hashCode()};" +
        "${Texts(arrayOf("a", null)).hashCode()};${Nested(arrayOf(Inner(5))).hashCode()};" +
        "${Mixed("m", intArrayOf(8), 9).hashCode()};${ints == Ints(shared)};" +
        "${Ints(intArrayOf(1)) == Ints(intArrayOf(1))}"
    val expected = "990;1176;1;34;0;3968;36;105967;true;false"
    return if (text == expected) "OK" else text
}
