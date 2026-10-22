// A class the runtime sizes through its Collection.size getter is also called through an interface
// of the program: the two descriptor entries are separate, so neither reads the other's pointer.
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
    override fun containsAll(elements: Collection<Int>): Boolean = elements.all { contains(it) }
    override fun iterator(): Iterator<Int> = listOf(1, 2).iterator()
    override fun count(): Int = 40
}

fun makeValues(): Iterable<Int> = Values()

fun asCounted(values: Any): Counted = values as Counted

fun box(): String {
    val values = makeValues()
    val counted = asCounted(values)
    val mapped = values.map { it + counted.count() }
    if (mapped != listOf(41, 42)) return "values $mapped"
    if (sizeReads != 1) return "size read $sizeReads times"
    return "OK"
}
