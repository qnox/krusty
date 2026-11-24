// KOTLINC_VERSION: [2.5.0,)
// kotlinc 2.4.20 and 2.4.21 emit an unloadable interface field and earlier releases refuse the
// initializers: the defects test models both (InterfaceBlockPropertyInitializerProhibited,
// InterfaceBlockPropertyFieldIsPrivateStatic).
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
