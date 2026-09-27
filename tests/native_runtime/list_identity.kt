// Kotlin's answers for `list_identity.c`: what a list is, and what it equals. `T` and `P` are the
// program classes the driver's `Tag` and `SeqList` (`program_collections.h`) stand in for, and `Q`
// its `Seq`: an element and a list that log each call made into them, and an `Iterable` that is
// not a list.

val log = StringBuilder()

class T(val n: Int) {
    override fun equals(other: Any?): Boolean {
        log.append("eq($n) ")
        return other is T && other.n == n
    }

    override fun hashCode() = n
    override fun toString() = "T$n"
}

class P(private vararg val xs: T) : List<T> {
    override val size: Int get() = xs.size
    override fun isEmpty() = xs.isEmpty()
    override fun contains(element: T) = xs.contains(element)
    override fun containsAll(elements: Collection<T>) = elements.all { xs.contains(it) }
    override fun get(index: Int) = xs[index]
    override fun indexOf(element: T) = xs.indexOf(element)
    override fun lastIndexOf(element: T) = xs.lastIndexOf(element)
    override fun iterator(): Iterator<T> {
        log.append("iterator ")
        return walk(0)
    }
    override fun listIterator(): ListIterator<T> = walk(0)
    override fun listIterator(index: Int): ListIterator<T> = walk(index)
    override fun subList(fromIndex: Int, toIndex: Int) = xs.toList().subList(fromIndex, toIndex)

    private fun walk(start: Int) = object : ListIterator<T> {
        var at = start
        override fun hasNext(): Boolean {
            log.append("hasNext ")
            return at < xs.size
        }
        override fun next(): T {
            log.append("next ")
            return xs[at++]
        }
        override fun hasPrevious() = at > 0
        override fun previous() = xs[--at]
        override fun nextIndex() = at
        override fun previousIndex() = at - 1
    }
}

class Q(private vararg val xs: T) : Iterable<T> {
    override fun iterator(): Iterator<T> {
        log.append("iterator ")
        return xs.iterator()
    }
}

fun box(): String = buildString {
    val readOnly: Any = listOf(T(1), T(2))
    val growable: Any = mutableListOf(T(1), T(2))
    appendLine("mutableListOf: ${growable is List<*>} ${growable is Collection<*>} " +
        "${growable is Iterable<*>} ${growable is RandomAccess} ${growable is MutableList<*>} " +
        "${growable is MutableCollection<*>} ${growable is MutableIterable<*>}")
    appendLine("listOf: ${readOnly is List<*>} ${readOnly is Collection<*>} " +
        "${readOnly is Iterable<*>} ${readOnly is RandomAccess} ${readOnly is MutableList<*>} " +
        "${readOnly is MutableCollection<*>} ${readOnly is MutableIterable<*>}")
    appendLine("mutableListOf::class.simpleName ${growable::class.simpleName}")
    appendLine("listOf::class.simpleName ${readOnly::class.simpleName}")
    fun equal(label: String, answer: Boolean) {
        appendLine("$label $answer | $log")
        log.setLength(0)
    }
    equal("listOf(1, 2) == P(1, 2)", readOnly == P(T(1), T(2)))
    equal("listOf(1, 2) == P(1, 2, 3)", readOnly == P(T(1), T(2), T(3)))
    equal("listOf(1, 2) == P(1)", readOnly == P(T(1)))
    equal("listOf(1, 2) == P(1, 3)", readOnly == P(T(1), T(3)))
    equal("listOf(1, 2) == Q(1, 2)", readOnly == Q(T(1), T(2)))
    equal("listOf() == P()", listOf<T>() == P())
    equal("mutableListOf(1, 2) == P(1, 2)", growable == P(T(1), T(2)))
    equal("mutableListOf(1, 2) == P(1, 2, 3)", growable == P(T(1), T(2), T(3)))
    equal("mutableListOf(1, 2) == P(1)", growable == P(T(1)))
    equal("mutableListOf(1, 2) == P(1, 3)", growable == P(T(1), T(3)))
    equal("listOf(1, 2) == mutableListOf(1, 2)", readOnly == growable)
    equal("mutableListOf(1, 2) == listOf(1, 2)", growable == readOnly)
}
