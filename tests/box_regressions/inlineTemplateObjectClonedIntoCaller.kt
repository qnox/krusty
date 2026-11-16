// FILE: main.kt
fun box(): String = make() + "K"

// FILE: lib.kt
interface I { fun run(): String }
inline fun make(): String = object : I { override fun run() = "O" }.run()
