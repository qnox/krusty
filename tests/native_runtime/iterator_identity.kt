@file:OptIn(ExperimentalUnsignedTypes::class)

// Kotlin's answers for `iterator_identity.c`: what each iterator the runtime hands out is -- its
// superclass, which of `Iterator` and the abstract primitive iterators an `is` finds it to be,
// and, where both platforms agree on it, its qualified class name. On the JVM `kotlin.Any` is
// `java.lang.Object`; the line names it as Kotlin does.
//
// Not compared here, and pinned by the driver: the class name of an array's iterator, which is
// platform-defined. The JVM's `intArrayOf().iterator()` is `kotlin.jvm.internal.ArrayIntIterator`
// and `arrayOf(1).iterator()` `kotlin.jvm.internal.ArrayIterator`; Kotlin/Native's are
// `kotlin.IntArrayIterator` and `kotlin.ArrayIterator` (and kin), and this runtime is the native
// one. A list's iterator is platform-defined the same way (`java.util.ArrayList.Itr` on the JVM).
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
    fun show(label: String, x: Any, named: Boolean) {
        val name = if (named) "${x::class.qualifiedName} " else ""
        appendLine("$label: ${name}super=${superName(x)} is ${kinds(x)}")
    }
    show("(1..2).iterator()", (1..2).iterator(), true)
    show("(2 downTo 1).iterator()", (2 downTo 1).iterator(), true)
    show("(1L..2L).iterator()", (1L..2L).iterator(), true)
    show("('a'..'b').iterator()", ('a'..'b').iterator(), true)
    show("(1u..2u).iterator()", (1u..2u).iterator(), true)
    show("(1uL..2uL).iterator()", (1uL..2uL).iterator(), true)
    show("arrayOf(1).iterator()", arrayOf<Any?>(1).iterator(), false)
    show("BooleanArray(1).iterator()", BooleanArray(1).iterator(), false)
    show("ByteArray(1).iterator()", ByteArray(1).iterator(), false)
    show("CharArray(1).iterator()", CharArray(1).iterator(), false)
    show("ShortArray(1).iterator()", ShortArray(1).iterator(), false)
    show("IntArray(1).iterator()", IntArray(1).iterator(), false)
    show("LongArray(1).iterator()", LongArray(1).iterator(), false)
    show("FloatArray(1).iterator()", FloatArray(1).iterator(), false)
    show("DoubleArray(1).iterator()", DoubleArray(1).iterator(), false)
    show("UByteArray(1).iterator()", UByteArray(1).iterator(), true)
    show("UShortArray(1).iterator()", UShortArray(1).iterator(), true)
    show("UIntArray(1).iterator()", UIntArray(1).iterator(), true)
    show("ULongArray(1).iterator()", ULongArray(1).iterator(), true)
    show("\"ab\".iterator()", "ab".iterator(), true)
    show("StringBuilder(\"ab\").iterator()", StringBuilder("ab").iterator(), true)
    show("listOf(1).iterator()", listOf(1).iterator(), false)
    show("mutableListOf(1).iterator()", mutableListOf(1).iterator(), false)
}
