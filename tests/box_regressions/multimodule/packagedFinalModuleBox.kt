// MODULE: library
// FILE: Library.kt
package packageentry.library

fun answer(): String = "OK"
fun box(): String = "library box"

// MODULE: main(library)
// FILE: Helper.kt
package packageentry.helper

class Decoy {
    fun box(): String = "member box"
}

// FILE: Main.kt
/* package wrong; fun box(): String = "comment box" */
package packageentry.`when`

import packageentry.library.answer

fun box(): String = answer()
