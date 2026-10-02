// Kotlin's answers for `iterator_exhausted.c`: `next()` on an exhausted iterator, through the
// general protocol (`next()` on an `Iterator<Any?>`, a boxed element) and, where the iterator has
// one, the narrow one (`nextInt()`, `nextChar()`, or `next()` on the concrete unsigned iterator).
fun box(): String = buildString {
    fun t(label: String, next: () -> Any?) {
        val answer = try {
            "answered ${next()}"
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label $answer")
    }
    // `elements` elements, then one more `next()` through each protocol, and a second one when
    // `again` says so: reading text past its end is `get(index++)`, which moves the index on even
    // though the read throws, so the second asks for the index after.
    fun general(label: String, elements: Int, again: Boolean = false, make: () -> Iterator<Any?>) {
        val walk = make()
        repeat(elements) { walk.next() }
        t("$label general") { walk.next() }
        if (again) t("$label general again") { walk.next() }
    }
    fun <I : Iterator<*>> narrow(
        label: String, elements: Int, make: () -> I, again: Boolean = false, next: (I) -> Any?,
    ) {
        val walk = make()
        repeat(elements) { next(walk) }
        t("$label narrow") { next(walk) }
        if (again) t("$label narrow again") { next(walk) }
    }

    general("arrayOf(5)", 1) { arrayOf<Any?>(5).iterator() }
    general("IntArray(1)", 1, again = true) { IntArray(1).iterator() }
    narrow("IntArray(1)", 1, { IntArray(1).iterator() }, again = true) { it.nextInt() }
    general("CharArray(2)", 2) { CharArray(2).iterator() }
    narrow("CharArray(2)", 2, { CharArray(2).iterator() }) { it.nextChar() }

    general("\"a\"", 1, again = true) { "a".iterator() }
    narrow("\"a\"", 1, { "a".iterator() }, again = true) { it.nextChar() }
    general("\"\"", 0) { "".iterator() }
    narrow("\"\"", 0, { "".iterator() }) { it.nextChar() }
    general("StringBuilder(\"a\")", 1, again = true) { StringBuilder("a").iterator() }
    narrow("StringBuilder(\"a\")", 1, { StringBuilder("a").iterator() }, again = true) {
        it.nextChar()
    }

    general("1..1", 1) { (1..1).iterator() }
    narrow("1..1", 1, { (1..1).iterator() }) { it.nextInt() }
    general("1L..1L", 1) { (1L..1L).iterator() }
    narrow("1L..1L", 1, { (1L..1L).iterator() }) { it.nextLong() }
    general("'a'..'a'", 1) { ('a'..'a').iterator() }
    narrow("'a'..'a'", 1, { ('a'..'a').iterator() }) { it.nextChar() }
    general("1u..1u", 1) { (1u..1u).iterator() }
    narrow("1u..1u", 1, { (1u..1u).iterator() }) { it.next() }
    general("2 downTo 1", 2) { (2 downTo 1).iterator() }
    narrow("2 downTo 1", 2, { (2 downTo 1).iterator() }) { it.nextInt() }

    general("listOf(1)", 1) { listOf<Any?>(1).iterator() }
    general("listOf()", 0) { listOf<Any?>().iterator() }
    general("mutableListOf(1)", 1) { mutableListOf<Any?>(1).iterator() }
    general("listOf(1).withIndex()", 1) { listOf(1).withIndex().iterator() }
}
