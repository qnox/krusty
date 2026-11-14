//! A Java method that realizes a type-safe Kotlin collection declaration takes that declaration's
//! parameter types. `java.util.Deque` redeclares `contains(Object)` and `remove(Object)`; kotlinc
//! reads them as `contains(element: E)` and `remove(element: E)`, the overrides of
//! `Collection.contains` and `MutableCollection.remove`, so a Kotlin class implementing or
//! delegating `Deque<E>` has one `contains` and one `remove(E)` obligation, not a raw Java one
//! beside each Kotlin one.

use super::common;

/// Delegation forwards each obligation once: no `contains(Any!)` forwarder beside `contains(E)`.
#[test]
fn a_deque_delegation_forwards_its_type_safe_members_once_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        r#"import java.util.ArrayDeque

abstract class Base<E> : AbstractMutableCollection<E>()

class Items<E>(private val items: ArrayDeque<E>) : Base<E>(), java.util.Deque<E> by items

class Plain<E>(private val items: ArrayDeque<E>) : java.util.Deque<E> by items

fun box(): String {
    val items = Items(ArrayDeque<String>())
    items.add("a")
    items.push("b")
    if (!items.contains("a") || !items.remove("a") || items.size != 1) return "Items $items"
    val plain = Plain(ArrayDeque<String>())
    plain.add("x")
    plain.addFirst("y")
    val collection: MutableCollection<String> = plain
    if (!collection.contains("x") || !collection.remove("x")) return "Plain collection"
    return if (plain.contains("y") && !plain.remove("x") && plain.size == 1) "OK" else "Plain $plain"
}
"#,
        "java_type_safe_collection_override_deque_delegation",
    );
}

/// A hand-written implementor overrides `contains(element: E)` and `remove(element: E)`, the only
/// `contains`/`remove(E)` obligations `Deque<E>` has.
#[test]
fn a_deque_implementor_overrides_its_type_safe_members_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        r#"import java.util.ArrayDeque

class Stack<E> : java.util.AbstractQueue<E>(), java.util.Deque<E> {
    private val items = ArrayDeque<E>()
    override val size: Int get() = items.size
    override fun iterator(): MutableIterator<E> = items.iterator()
    override fun descendingIterator(): MutableIterator<E> = items.descendingIterator()
    override fun offer(e: E): Boolean = items.offer(e)
    override fun poll(): E? = items.poll()
    override fun peek(): E? = items.peek()
    override fun addFirst(e: E) = items.addFirst(e)
    override fun addLast(e: E) = items.addLast(e)
    override fun offerFirst(e: E) = items.offerFirst(e)
    override fun offerLast(e: E) = items.offerLast(e)
    override fun removeFirst(): E = items.removeFirst()
    override fun removeLast(): E = items.removeLast()
    override fun pollFirst(): E? = items.pollFirst()
    override fun pollLast(): E? = items.pollLast()
    override fun getFirst(): E = items.getFirst()
    override fun getLast(): E = items.getLast()
    override fun peekFirst(): E? = items.peekFirst()
    override fun peekLast(): E? = items.peekLast()
    override fun removeFirstOccurrence(o: Any?): Boolean = items.removeFirstOccurrence(o)
    override fun removeLastOccurrence(o: Any?): Boolean = items.removeLastOccurrence(o)
    override fun push(e: E) = items.push(e)
    override fun pop(): E = items.pop()
    override fun contains(element: E): Boolean = items.contains(element)
    override fun remove(element: E): Boolean = items.remove(element)
}

fun box(): String {
    val stack = Stack<String>()
    stack.add("x")
    stack.push("y")
    if (!stack.contains("x") || !stack.remove("x") || stack.size != 1) return "members $stack"
    @Suppress("UNCHECKED_CAST")
    val erased = stack as MutableCollection<Any?>
    if (erased.contains(1) || erased.remove(1)) return "bridges"
    return if (stack.pop() == "y" && stack.isEmpty()) "OK" else "pop $stack"
}
"#,
        "java_type_safe_collection_override_deque_implementor",
    );
}
