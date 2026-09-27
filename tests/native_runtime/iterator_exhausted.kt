// Kotlin's answers for `iterator_exhausted.c`: `next()` on an exhausted iterator, through the
// general protocol (`next()` on an `Iterator<Any?>`, a boxed element) and, where the iterator has
// one, the narrow one (`nextInt()`, `nextChar()`, or `next()` on the concrete unsigned iterator).
//
// An exception is named by the Kotlin class a program catches it as: reading text past its end
// throws the JVM's `StringIndexOutOfBoundsException`, a platform subclass of Kotlin's
// `IndexOutOfBoundsException`. An array iterator's message is platform-defined -- the JVM's is
// `Index 1 out of bounds for length 1`, Kotlin/Native's the index alone, `1` -- so for an array
// only the class is compared, and the driver pins the native message.
fun name(e: Throwable): String? =
    if (e is IndexOutOfBoundsException) "IndexOutOfBoundsException" else e::class.simpleName

fun box(): String = buildString {
    fun t(label: String, message: Boolean, next: () -> Any?) {
        val answer = try {
            "answered ${next()}"
        } catch (e: Throwable) {
            "threw ${name(e)}" + if (message) ": ${e.message}" else ""
        }
        appendLine("$label $answer")
    }
    // `elements` elements, then one more `next()` through each protocol, and a second one when
    // `again` says so: reading text past its end is `get(index++)`, which moves the index on even
    // though the read throws, so the second asks for the index after.
    fun general(label: String, message: Boolean, elements: Int, again: Boolean = false,
                make: () -> Iterator<Any?>) {
        val walk = make()
        repeat(elements) { walk.next() }
        t("$label general", message) { walk.next() }
        if (again) t("$label general again", message) { walk.next() }
    }
    fun <I : Iterator<*>> narrow(label: String, message: Boolean, elements: Int, make: () -> I,
                                 again: Boolean = false, next: (I) -> Any?) {
        val walk = make()
        repeat(elements) { next(walk) }
        t("$label narrow", message) { next(walk) }
        if (again) t("$label narrow again", message) { next(walk) }
    }

    general("arrayOf(5)", false, 1) { arrayOf<Any?>(5).iterator() }
    general("IntArray(1)", false, 1, again = true) { IntArray(1).iterator() }
    narrow("IntArray(1)", false, 1, { IntArray(1).iterator() }, again = true) { it.nextInt() }
    general("CharArray(2)", false, 2) { CharArray(2).iterator() }
    narrow("CharArray(2)", false, 2, { CharArray(2).iterator() }) { it.nextChar() }

    general("\"a\"", true, 1, again = true) { "a".iterator() }
    narrow("\"a\"", true, 1, { "a".iterator() }, again = true) { it.nextChar() }
    general("\"\"", true, 0) { "".iterator() }
    narrow("\"\"", true, 0, { "".iterator() }) { it.nextChar() }
    general("StringBuilder(\"a\")", true, 1, again = true) { StringBuilder("a").iterator() }
    narrow("StringBuilder(\"a\")", true, 1, { StringBuilder("a").iterator() },
        again = true) { it.nextChar() }

    general("1..1", true, 1) { (1..1).iterator() }
    narrow("1..1", true, 1, { (1..1).iterator() }) { it.nextInt() }
    general("1L..1L", true, 1) { (1L..1L).iterator() }
    narrow("1L..1L", true, 1, { (1L..1L).iterator() }) { it.nextLong() }
    general("'a'..'a'", true, 1) { ('a'..'a').iterator() }
    narrow("'a'..'a'", true, 1, { ('a'..'a').iterator() }) { it.nextChar() }
    general("1u..1u", true, 1) { (1u..1u).iterator() }
    narrow("1u..1u", true, 1, { (1u..1u).iterator() }) { it.next() }
    general("2 downTo 1", true, 2) { (2 downTo 1).iterator() }
    narrow("2 downTo 1", true, 2, { (2 downTo 1).iterator() }) { it.nextInt() }

    general("listOf(1)", true, 1) { listOf<Any?>(1).iterator() }
    general("listOf()", true, 0) { listOf<Any?>().iterator() }
    general("mutableListOf(1)", true, 1) { mutableListOf<Any?>(1).iterator() }
    general("listOf(1).withIndex()", true, 1) { listOf(1).withIndex().iterator() }
}
