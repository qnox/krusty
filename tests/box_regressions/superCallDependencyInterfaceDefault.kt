// MODULE: lib
// FILE: Bases.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

open class Base<T> {
    open fun put(value: T): String = "base:$value"
}

interface Sink<T> {
    fun accept(value: T): String = "sink:$value"
}

// MODULE: box(lib)
// FILE: box.kt
class Sub : Base<Id>() {
    override fun put(value: Id): String = "sub+" + super.put(value)
}

class Taker : Sink<Id> {
    override fun accept(value: Id): String = "taker+" + super.accept(value)
}

fun box(): String {
    val put = Sub().put(Id(1))
    if (put != "sub+base:Id(raw=1)") return "class super: $put"
    val accepted = Taker().accept(Id(2))
    if (accepted != "taker+sink:Id(raw=2)") return "interface super: $accepted"
    return "OK"
}
