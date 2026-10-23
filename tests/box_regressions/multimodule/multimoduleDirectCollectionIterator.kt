// MODULE: lib
// FILE: values.kt
@OptIn(ExperimentalUnsignedTypes::class)
fun unsignedValues(): Iterable<UByte> = ubyteArrayOf(1u, 255u)
// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    val iterator = unsignedValues().iterator()
    if (!iterator.hasNext() || iterator.next() != 1.toUByte()) return "first"
    if (!iterator.hasNext() || iterator.next() != 255.toUByte()) return "second"
    if (iterator.hasNext()) return "end"
    return "OK"
}
