// A class whose descriptor carries a Collection.size thunk is also called through an interface of
// the program, in one compilation: the two descriptor entries are separate fields.
var sizeReads = 0

interface Counted {
    fun count(): Int
}

class Values : Collection<Int>, Counted {
    override val size: Int
        get() {
            sizeReads++
            return 2
        }
    override fun isEmpty(): Boolean = false
    override fun contains(element: Int): Boolean = element == 1 || element == 2
    override fun containsAll(elements: Collection<Int>): Boolean = false
    override fun iterator(): Iterator<Int> = listOf(1, 2).iterator()
    override fun count(): Int = 40
}

fun countOf(counted: Counted): Int = counted.count()

fun box(): String {
    val values = Values()
    val sum = values.size + countOf(values)
    if (sum != 42) return "sum $sum"
    if (sizeReads != 1) return "size read $sizeReads times"
    return "OK"
}
