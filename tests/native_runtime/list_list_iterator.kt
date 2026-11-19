// Kotlin's answers for `list_list_iterator.c`: a read-only list's `listIterator(index)` walked
// backwards and forwards, and whether stepping before the start, or opening one past either end,
// raises.
fun box(): String = buildString {
    val list = listOf(1, 2)
    val iterator = list.listIterator(2)
    appendLine("listOf(1, 2).listIterator(2): ${iterator.hasPrevious()} ${iterator.nextIndex()} ${iterator.previousIndex()}")
    appendLine("previous ${iterator.previous()} ${iterator.previous()} ${iterator.hasPrevious()}")
    appendLine("previous at the start threw ${try { iterator.previous(); false } catch (e: NoSuchElementException) { true }}")
    appendLine("next ${iterator.next()} ${iterator.next()} ${iterator.hasNext()}")
    for (index in intArrayOf(-1, 3)) {
        appendLine("listIterator($index) threw ${try { list.listIterator(index); false } catch (e: IndexOutOfBoundsException) { true }}")
    }
}
