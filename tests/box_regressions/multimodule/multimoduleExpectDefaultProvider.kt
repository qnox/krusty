// MODULE: common
expect fun answer(value: String = "OK"): String
// MODULE: main()()(common)
// FILE: call.kt
fun box() = answer()
// FILE: actual.kt
actual fun answer(value: String) = value
