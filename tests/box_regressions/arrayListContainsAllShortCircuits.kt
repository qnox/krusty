// WITH_STDLIB

// `containsAll` reads its argument one element at a time and answers false at the first element
// the receiver lacks, so a later advance of the argument's iterator is never observed.
class Advances {
    var count = 0
}

class ThrowingIterator(private val advances: Advances) : Iterator<String> {
    override fun hasNext(): Boolean = true
    override fun next(): String {
        advances.count++
        if (advances.count > 1) throw IllegalStateException("advanced past the first element")
        return "missing"
    }
}

class ThrowsOnSecond(private val advances: Advances) : Collection<String> {
    override val size: Int get() = 2
    override fun isEmpty(): Boolean = false
    override fun contains(element: String): Boolean = true
    override fun containsAll(elements: Collection<String>): Boolean = true
    override fun iterator(): Iterator<String> = ThrowingIterator(advances)
}

class CountingIterator(private val inner: Iterator<String>, private val advances: Advances) :
    Iterator<String> {
    override fun hasNext(): Boolean = inner.hasNext()
    override fun next(): String {
        advances.count++
        return inner.next()
    }
}

class Counting(private val items: List<String>, private val advances: Advances) :
    Collection<String> {
    override val size: Int get() = items.size
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun contains(element: String): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<String>): Boolean = items.containsAll(elements)
    override fun iterator(): Iterator<String> = CountingIterator(items.iterator(), advances)
}

fun box(): String {
    val receiver = ArrayList<String>()
    receiver.add("a")
    receiver.add("b")

    val first = Advances()
    if (receiver.containsAll(ThrowsOnSecond(first))) return "missing first element answered true"
    if (first.count != 1) return "advanced ${first.count} times past a missing element"

    val middle = Advances()
    if (receiver.containsAll(Counting(listOf("a", "x", "b"), middle))) {
        return "missing middle element answered true"
    }
    if (middle.count != 2) return "stopped after ${middle.count} advances, expected 2"

    val present = Advances()
    if (!receiver.containsAll(Counting(listOf("b", "a"), present))) {
        return "present elements answered false"
    }
    if (present.count != 2) return "present elements advanced ${present.count} times"

    if (!receiver.containsAll(ArrayList<String>())) return "empty argument answered false"
    val runtime = ArrayList<String>()
    runtime.add("a")
    runtime.add("z")
    if (receiver.containsAll(runtime)) return "runtime list with a missing element answered true"
    return "OK"
}
