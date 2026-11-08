// MODULE: lib
// FILE: Act.kt
package fixture

class Act(first: () -> Unit, second: () -> Unit) {
    init {
        first()
        second()
    }
}

// MODULE: main(lib)
// FILE: First.kt
package fixture

var text = ""
fun firstConstruction() {
    Act({ text += "O" }, { text += "K" })
}

// FILE: Consumer.kt
package fixture

fun box(): String {
    firstConstruction()
    if (text != "OK") return "first:$text"
    Act({ text += "O" }, { text += "K" })
    return if (text == "OKOK") "OK" else "second:$text"
}
