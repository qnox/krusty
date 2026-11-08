// MODULE: lib
// FILE: Lib.kt
package fixture

enum class Shade { LIGHT, DARK }

fun dark(): Shade = Shade.DARK

// MODULE: main(lib)
// FILE: Main.kt
package fixture

fun box(): String {
    val first = dark()
    val again = dark()
    if (first.ordinal != 1) return "ordinal=${first.ordinal}"
    if (first.name != "DARK") return "name=${first.name}"
    if (first !== again) return "identity"
    return "OK"
}
