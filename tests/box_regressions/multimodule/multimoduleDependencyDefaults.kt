// MODULE: lib
// FILE: lib.kt
package lib

val tail = "K"

fun answer(prefix: String = "O", value: String = prefix + tail) = value

fun String.suffixed(suffix: CharSequence = tail, times: Int = 1): String {
    var result = this
    for (i in 0 until times) result += suffix
    return result
}

// MODULE: main(lib)
// FILE: main.kt
import lib.*

fun box(): String {
    if (answer() != "OK") return "fail answer(): ${answer()}"
    if (answer("X") != "XK") return "fail answer(X)"
    if (answer(value = "V") != "V") return "fail answer(value)"
    if ("O".suffixed() != "OK") return "fail suffixed()"
    if ("O".suffixed(times = 2) != "OKK") return "fail suffixed(times)"
    if ("O".suffixed("k") != "Ok") return "fail suffixed(k)"
    return "OK"
}
