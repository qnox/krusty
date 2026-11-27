// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 fails in IR lowering on this program, so it cannot be the oracle.
// LANGUAGE: +CompanionBlocks +CompanionExtensions
// A source `companion fun/val C.name` is named through `C` only once its package brings it into
// scope, as a dependency's is: an explicit import of the name, or a star import of the package. The
// declaration of another package that nothing imports is not a candidate, so `pick` below is not
// ambiguous.

// FILE: a/C.kt
package a

class C

companion fun C.pick(): String = "O"

companion val C.answer: Int get() = 42

// FILE: unimported/Other.kt
package unimported

import a.C

companion fun C.pick(): String = "unimported"

companion val C.answer: Int get() = -1

// FILE: b/Starred.kt
package b

import a.C

companion fun C.fromStar(): String = "K"

// FILE: main.kt
import a.C
import a.answer
import a.pick
import b.*

fun box(): String {
    if (C.answer != 42) return "Fail answer: ${C.answer}"
    if (C.pick() != "O") return "Fail pick: ${C.pick()}"
    return C.pick() + C.fromStar()
}
