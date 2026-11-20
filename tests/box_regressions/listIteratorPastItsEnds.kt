// A list iterator has no element before its first position or after its last: `previous()` on
// an empty list's iterator, `previous()` at the start of a nonempty list and `next()` at its end
// throw `NoSuchElementException`, which a Kotlin `catch` observes. Within the list, `previous()`
// returns the element before the iterator.
fun noSuchElement(step: () -> String): Boolean =
    try {
        step()
        false
    } catch (e: NoSuchElementException) {
        true
    }

fun box(): String {
    val empty = ArrayList<String>()
    if (!noSuchElement { empty.listIterator().previous() }) return "FAIL empty previous"
    val list = ArrayList<String>()
    list.add("K")
    if (!noSuchElement { list.listIterator().previous() }) return "FAIL previous before the first"
    if (list.listIterator(1).previous() != "K") return "FAIL previous within"
    val iterator = list.iterator()
    if (iterator.next() != "K") return "FAIL next within"
    if (!noSuchElement { iterator.next() }) return "FAIL next past the last"
    return "OK"
}
