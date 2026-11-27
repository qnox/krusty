// MODULE: library
// FILE: Library.kt
package fixture

val names: Array<String> = arrayOf("O", "K")

fun pair(first: String, second: String): Array<String> = arrayOf(first, second)

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val built = pair("O", "K")
    return if (names.size == 2 && names[0] + names[1] == "OK" && built[0] + built[1] == "OK") "OK" else "FAIL"
}
