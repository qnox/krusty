// A property of the current package outranks a same-named enum entry a star import offers.

// FILE: second/SomeEnum.kt
package second

enum class SomeEnum { SOME; }

// FILE: main.kt
import second.SomeEnum.*

val SOME = "OK"

fun box(): String = SOME
