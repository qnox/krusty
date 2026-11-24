// KOTLINC_VERSION: [2.5.0,)
// Earlier JVM reference compilers fail during IR lowering of the consumer module.
// MODULE: lib
// FILE: Lib.kt
// LANGUAGE: +CompanionBlocks
interface Source {
    companion {
        var current: String = "O"
        val fixed: String = "K"
    }
}

// MODULE: main(lib)
// FILE: Main.kt
// LANGUAGE: +CompanionBlocks
fun box(): String {
    val before = Source.current
    Source.current = before + "!"
    if (Source.current != "O!") return "FAIL: ${Source.current}"
    Source.current = "O"
    return Source.current + Source.fixed
}
