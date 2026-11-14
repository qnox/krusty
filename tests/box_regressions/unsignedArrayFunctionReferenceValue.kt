@file:OptIn(ExperimentalUnsignedTypes::class)
fun id(a: UIntArray) = a
fun box(): String {
    val f = ::id
    val r = f(uintArrayOf(42u))
    return if (r[0] == 42u) "OK" else "fail " + r[0]
}
