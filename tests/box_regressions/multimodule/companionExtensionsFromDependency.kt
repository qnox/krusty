// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 fails in IR lowering on this program, so it cannot be the oracle.
// LANGUAGE: +CompanionBlocks +CompanionExtensions
// A dependency's written `companion fun/val/var C.name` is named through `C` once its package
// brings it into scope: an explicit import of the name, or a star import of the package.

// MODULE: lib
// FILE: a/C.kt
package a

class C

private var stored = 1

companion fun C.greet(suffix: String): String = "O$suffix"

companion val C.answer: Int get() = 42

companion var C.counter: Int
    get() = stored
    set(value) {
        stored = value
    }

// FILE: b/Starred.kt
package b

import a.C

companion fun C.fromStar(): String = "star"

// MODULE: main(lib)
// FILE: main.kt
import a.C
import a.answer
import a.counter
import a.greet
import b.*

fun box(): String {
    if (C.answer != 42) return "Fail answer: ${C.answer}"
    C.counter += 2
    if (C.counter != 3) return "Fail counter: ${C.counter}"
    if (C.fromStar() != "star") return "Fail star: ${C.fromStar()}"
    return C.greet("K")
}
