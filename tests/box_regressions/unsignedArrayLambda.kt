@file:OptIn(ExperimentalUnsignedTypes::class)
fun box(): String {
    val f: (UIntArray) -> UIntArray = { it }
    val r = f(uintArrayOf(42u))
    return if (r[0] == 42u) "OK" else "fail " + r[0]
}
