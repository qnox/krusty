// WITH_STDLIB
OPTIONAL_JVM_INLINE_ANNOTATION
value class First(val value: Int)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Second(val value: Int)
fun box(): String {
    val first: Any = First(7)
    val second: Any = Second(7)
    return if (first != second) "OK" else "different declared value classes compared equal"
}
