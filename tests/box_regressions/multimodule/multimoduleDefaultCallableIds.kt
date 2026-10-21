// MODULE: lib
// FILE: libRead.kt
fun read() = answer()
// FILE: libDefault.kt
fun answer(prefix: String = "O", value: String = prefix + "K") = value
// MODULE: main(lib)
// FILE: mainBox.kt
fun box(): String { local(); return read() }
// FILE: mainDefault.kt
fun local(prefix: String = "lo", value: String = prefix + "cal") = value
