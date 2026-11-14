fun ints(ys: IntArray): Int {
    fun sum(vararg n: Int): Int = n[0] + n[n.size - 1]
    fun at(vararg n: Int, index: Int = 1): Int = n[index] * 10
    return sum(*ys) + sum(1, *ys) + at(*ys) + at(7, *ys, index = 2) + at(4, 5, index = 0)
}
fun box(): String = if (ints(intArrayOf(2, 3)) == 5 + 4 + 30 + 30 + 40) "OK" else "Fail"
