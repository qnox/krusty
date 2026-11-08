// A value class read out of a generic slot and then discarded is popped as the slot holds it.
// A Long carrier is still that one reference pop, not pop2 of the unboxed long.
// A value the statement keeps is still unboxed.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Foo(val value: Int)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Name(val text: String)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Span(val value: Long)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Slot(private val holder: Any?) {
    val size: Int get() = holder as Int
}
fun <T> id(x: T): T = x
fun <T> T.idExtension(): T = this
fun discard(foo: Foo, name: Name, span: Span) {
    id(foo)
    foo.idExtension()
    id(foo).idExtension()
    id(name)
    id(span)
}
fun replace(slots: ArrayList<Slot>): Int {
    slots[0] = Slot(2)
    return slots[0].size
}
fun use(foo: Foo): Int {
    val kept = id(foo)
    return kept.value
}
fun box(): String {
    discard(Foo(1), Name("a"), Span(4L))
    if (replace(arrayListOf(Slot(1))) != 2) return "fail replace"
    if (use(Foo(3)) != 3) return "fail use"
    return "OK"
}
