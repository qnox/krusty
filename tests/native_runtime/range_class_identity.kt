// Kotlin's classes for `range_class_identity.c`: each range and progression with its rendering,
// then each iterator, with its qualified and simple class names and its superclass. On the JVM
// `kotlin.Any` is `java.lang.Object`; the line names it as Kotlin does.
fun superName(x: Any): String =
    x.javaClass.superclass.name.let { if (it == "java.lang.Object") "kotlin.Any" else it }

fun box(): String = buildString {
    val items = listOf<Any>(1..3, 1..3 step 1, 3 downTo 1, (1..3).reversed(), 1L..3L,
        1L..3L step 2, 'a'..'c', 'c' downTo 'a', 1u..3u, 1u..3u step 2, 1uL..3uL,
        3uL downTo 1uL)
    for (x in items) {
        appendLine("$x | ${x::class.qualifiedName} ${x::class.simpleName} super=${superName(x)}")
    }
    val iterators = listOf<Any>((1..3).iterator(), (3 downTo 1).iterator(), (1L..3L).iterator(),
        ('a'..'c').iterator(), (1u..3u).iterator(), (1uL..3uL).iterator())
    for (i in iterators) {
        appendLine("${i::class.qualifiedName} ${i::class.simpleName} super=${superName(i)}")
    }
}
