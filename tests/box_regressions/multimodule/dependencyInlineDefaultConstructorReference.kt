// A library inline function whose omitted function-valued parameter defaults to a constructor
// reference. The call site inlines the default, as kotlinc does.

// MODULE: library
// FILE: library.kt
package copies

class Remapper(val source: String)

inline fun <reified T : CharSequence> T.copyWith(
    prefix: String = "",
    create: (String) -> Remapper = ::Remapper,
): String = create(prefix + this).source

// MODULE: main(library)
// FILE: main.kt
import copies.copyWith

fun box(): String {
    val text: CharSequence = "K"
    val explicit = text.copyWith("O") { Remapper(it.lowercase()) }
    if (explicit != "ok") return "fail: explicit $explicit"
    return text.copyWith("O")
}
