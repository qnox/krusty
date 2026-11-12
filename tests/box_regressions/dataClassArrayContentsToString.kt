data class Ints(val xs: IntArray)
data class MaybeInts(val xs: IntArray?)
data class Texts(val xs: Array<String>)
data class MaybeTexts(val xs: Array<String?>?)
data class Boxed(val xs: Array<Int?>)
data class Inner(val n: Int)
data class Nested(val xs: Array<Inner>)
data class Mixed(val label: String, val xs: IntArray, val count: Int)

fun box(): String {
    val shared = intArrayOf(1, -2)
    val first = Ints(shared).toString()
    shared[0] = 7
    val text = first + ";" +
        Ints(shared).toString() + ";" +
        Ints(intArrayOf()).toString() + ";" +
        MaybeInts(intArrayOf(3)).toString() + ";" +
        MaybeInts(null).toString() + ";" +
        Texts(arrayOf("a", "b")).toString() + ";" +
        MaybeTexts(arrayOf("c", null)).toString() + ";" +
        MaybeTexts(null).toString() + ";" +
        Boxed(arrayOf(4, null)).toString() + ";" +
        Nested(arrayOf(Inner(5), Inner(6))).toString() + ";" +
        "in ${Mixed("m", intArrayOf(8), 9)}"
    val expected = "Ints(xs=[1, -2]);Ints(xs=[7, -2]);Ints(xs=[]);MaybeInts(xs=[3]);" +
        "MaybeInts(xs=null);Texts(xs=[a, b]);MaybeTexts(xs=[c, null]);MaybeTexts(xs=null);" +
        "Boxed(xs=[4, null]);Nested(xs=[Inner(n=5), Inner(n=6)]);in Mixed(label=m, xs=[8], count=9)"
    return if (text == expected) "OK" else text
}
