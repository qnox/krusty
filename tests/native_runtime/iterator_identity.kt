@file:OptIn(ExperimentalUnsignedTypes::class)

// Kotlin's answers for `iterator_identity.c`: what each iterator the runtime hands out is -- its
// qualified class name, its superclass, and which of `Iterator` and the abstract primitive
// iterators an `is` finds it to be. On the JVM `kotlin.Any` is `java.lang.Object`; the line names
// it as Kotlin does.
fun superName(x: Any): String =
    x.javaClass.superclass.name.let { if (it == "java.lang.Object") "kotlin.Any" else it }

fun kinds(x: Any): String = listOf(
    "Iterator" to (x is Iterator<*>),
    "BooleanIterator" to (x is BooleanIterator),
    "ByteIterator" to (x is ByteIterator),
    "CharIterator" to (x is CharIterator),
    "ShortIterator" to (x is ShortIterator),
    "IntIterator" to (x is IntIterator),
    "LongIterator" to (x is LongIterator),
    "FloatIterator" to (x is FloatIterator),
    "DoubleIterator" to (x is DoubleIterator),
).filter { it.second }.joinToString(" ") { it.first }

fun box(): String = buildString {
    fun show(label: String, x: Any) {
        appendLine("$label: ${x::class.qualifiedName} super=${superName(x)} is ${kinds(x)}")
    }
    show("(1..2).iterator()", (1..2).iterator())
    show("(2 downTo 1).iterator()", (2 downTo 1).iterator())
    show("(1L..2L).iterator()", (1L..2L).iterator())
    show("('a'..'b').iterator()", ('a'..'b').iterator())
    show("(1u..2u).iterator()", (1u..2u).iterator())
    show("(1uL..2uL).iterator()", (1uL..2uL).iterator())
    show("arrayOf(1).iterator()", arrayOf<Any?>(1).iterator())
    show("BooleanArray(1).iterator()", BooleanArray(1).iterator())
    show("ByteArray(1).iterator()", ByteArray(1).iterator())
    show("CharArray(1).iterator()", CharArray(1).iterator())
    show("ShortArray(1).iterator()", ShortArray(1).iterator())
    show("IntArray(1).iterator()", IntArray(1).iterator())
    show("LongArray(1).iterator()", LongArray(1).iterator())
    show("FloatArray(1).iterator()", FloatArray(1).iterator())
    show("DoubleArray(1).iterator()", DoubleArray(1).iterator())
    show("UByteArray(1).iterator()", UByteArray(1).iterator())
    show("UShortArray(1).iterator()", UShortArray(1).iterator())
    show("UIntArray(1).iterator()", UIntArray(1).iterator())
    show("ULongArray(1).iterator()", ULongArray(1).iterator())
    show("\"ab\".iterator()", "ab".iterator())
    show("StringBuilder(\"ab\").iterator()", StringBuilder("ab").iterator())
    show("listOf(1, 2).iterator()", listOf(1, 2).iterator())
    show("mutableListOf(1, 2).iterator()", mutableListOf(1, 2).iterator())
}
