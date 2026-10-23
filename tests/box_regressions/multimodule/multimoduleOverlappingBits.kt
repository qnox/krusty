// MODULE: bits
fun raw(value: Float): Int = value.toRawBits()
// MODULE: main(bits)
fun canonical(value: Float): Int = value.toBits()
fun box(): String {
    if (raw(1.0f) != 1065353216) return "raw"
    if (canonical(1.0f) != 1065353216) return "canonical"
    return "OK"
}
