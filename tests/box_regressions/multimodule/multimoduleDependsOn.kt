// MODULE: common
expect fun answer(): String
// MODULE: main()()(common)
actual fun answer() = "OK"
fun box() = answer()
