// ArrayList(Collection) makes a mutable copy, and walks an arbitrary Collection through its
// iterator. The selected Collection constructor is distinct from the initial-capacity overload.
class IteratedValues<E>(private val values: List<E>) : Collection<E> {
    var iteratorCalls = 0

    override val size: Int get() = values.size
    override fun isEmpty(): Boolean = values.isEmpty()
    override fun contains(element: E): Boolean = values.contains(element)
    override fun containsAll(elements: Collection<E>): Boolean = values.containsAll(elements)
    override fun iterator(): Iterator<E> {
        iteratorCalls++
        return values.iterator()
    }
}

var sourceEvaluations = 0

fun evaluateSource(source: Collection<String?>): Collection<String?> {
    sourceEvaluations++
    return source
}

fun box(): String {
    val empty = ArrayList<Int>(emptyList())
    if (empty.isNotEmpty()) return "empty copy"
    empty.add(7)
    if (empty.single() != 7) return "empty copy is not mutable"

    val source = mutableListOf<String?>("first", null)
    sourceEvaluations = 0
    val copy = ArrayList<String?>(evaluateSource(source))
    if (sourceEvaluations != 1) return "source evaluated $sourceEvaluations times"
    source[0] = "changed source"
    source.add("source only")
    copy.add("copy only")
    if (copy != listOf("first", null, "copy only")) return "copy contents $copy"
    if (source != listOf("changed source", null, "source only")) return "source contents $source"

    val marker = Any()
    val shallowCopy = ArrayList<Any>(mutableListOf(marker))
    if (shallowCopy.single() !== marker) return "copy changed element identity"

    val iterated = IteratedValues(listOf("x", "y"))
    val walked = ArrayList(iterated)
    if (iterated.iteratorCalls != 1) return "iterator calls ${iterated.iteratorCalls}"
    if (walked != listOf("x", "y")) return "iteration order $walked"
    walked[0] = "changed copy"
    if (iterated.contains("changed copy")) return "copy shares source storage"
    return "OK"
}
