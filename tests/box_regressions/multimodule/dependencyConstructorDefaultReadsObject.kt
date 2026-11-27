// MODULE: lib
package greeting

object Defaults {
    const val prefix = "O"
    val suffix = "K"
}

class Greeting(val head: String = Defaults.prefix, val tail: String = Defaults.suffix)
// MODULE: main(lib)
import greeting.Greeting

fun box(): String {
    val defaulted = Greeting()
    if (defaulted.head + defaulted.tail != "OK") return "Fail default: ${defaulted.head}${defaulted.tail}"
    val partial = Greeting("N")
    if (partial.head + partial.tail != "NK") return "Fail partial: ${partial.head}${partial.tail}"
    val full = Greeting("O", "K")
    return full.head + full.tail
}
