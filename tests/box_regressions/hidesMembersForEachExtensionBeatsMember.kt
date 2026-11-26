// WITH_STDLIB
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

class A {
    fun forEach(count: Int): A = this
}

// `forEach` is in the annotation's resolve name set, so this extension outranks the member even
// though the member applies without conversion.
@kotlin.internal.HidesMembers
fun A.forEach(count: Int): Int = count * 2

fun box(): String {
    val result: Int = A().forEach(21)
    return if (result == 42) "OK" else "FAIL: $result"
}
