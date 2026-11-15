// A subclass inherits the final special bridges its abstract collection superclass writes for
// members it leaves abstract, when another file declares that superclass: `size()` to the
// abstract `getSize()`, and the type-checking `contains(Object)` to the abstract
// `contains(String)`.
// FILE: base.kt
abstract class Sized<E> : MutableCollection<E>

abstract class Strings : MutableCollection<String>

// FILE: main.kt
class ListQueue<E> : Sized<E>() {
    private val items = ArrayList<E>()
    override val size: Int get() = items.size
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun contains(element: E): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<E>): Boolean = items.containsAll(elements)
    override fun iterator(): MutableIterator<E> = items.iterator()
    override fun add(element: E): Boolean = items.add(element)
    override fun addAll(elements: Collection<E>): Boolean = items.addAll(elements)
    override fun remove(element: E): Boolean = items.remove(element)
    override fun removeAll(elements: Collection<E>): Boolean = items.removeAll(elements)
    override fun retainAll(elements: Collection<E>): Boolean = items.retainAll(elements)
    override fun clear() = items.clear()
}

class StringQueue : Strings() {
    private val items = ArrayList<String>()
    override val size: Int get() = items.size
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun contains(element: String): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<String>): Boolean = items.containsAll(elements)
    override fun iterator(): MutableIterator<String> = items.iterator()
    override fun add(element: String): Boolean = items.add(element)
    override fun addAll(elements: Collection<String>): Boolean = items.addAll(elements)
    override fun remove(element: String): Boolean = items.remove(element)
    override fun removeAll(elements: Collection<String>): Boolean = items.removeAll(elements)
    override fun retainAll(elements: Collection<String>): Boolean = items.retainAll(elements)
    override fun clear() = items.clear()
}

fun box(): String {
    val queue: MutableCollection<String> = ListQueue()
    queue.add("O")
    val strings: MutableCollection<String> = StringQueue()
    strings.add("K")
    val any: Collection<Any?> = strings
    if (queue.size != 1 || strings.size != 1) return "FAIL size"
    if ("O" !in queue || 1 in any || "K" !in any) return "FAIL contains"
    return queue.first() + strings.first()
}
