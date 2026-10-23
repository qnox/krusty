// MODULE: lib
// FILE: libValue.kt
val answer = "OK"
// FILE: libRead.kt
fun read() = answer
// MODULE: main(lib)
// FILE: mainValue.kt
val local = "local"
// FILE: mainBox.kt
fun box(): String { if (local != "local") return "fail"; return read() }
