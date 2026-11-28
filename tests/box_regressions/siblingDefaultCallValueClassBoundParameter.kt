// FILE: Ids.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

fun <T : Id> topLevel(value: T, extra: Int = 3): Int = value.raw + extra

open class Holder {
    fun <T : Id> member(value: T, extra: Int = 5): Int = value.raw * extra
    fun <T : Id?> nullable(value: T, extra: Int = 2): Int = (value?.raw ?: -1) + extra
}

// FILE: Main.kt
class Derived : Holder()

fun box(): String {
    if (topLevel(Id(4)) != 7) return "top-level: ${topLevel(Id(4))}"
    if (topLevel(Id(4), 10) != 14) return "top-level explicit: ${topLevel(Id(4), 10)}"
    val derived = Derived()
    if (derived.member(Id(4)) != 20) return "inherited member: ${derived.member(Id(4))}"
    if (derived.nullable(Id(4)) != 6) return "inherited nullable: ${derived.nullable(Id(4))}"
    if (derived.nullable<Id?>(null) != 1) return "inherited null: ${derived.nullable<Id?>(null)}"
    return "OK"
}
