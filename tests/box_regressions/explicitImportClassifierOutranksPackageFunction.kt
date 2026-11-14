// An explicitly imported class sits on a nearer scope rung than a same-package function of the
// same name, so a bare call constructs it (kotlinc 2.4.21).
// FILE: other.kt
package other

class Tag(val first: Int, val second: Int)

// FILE: main.kt
package demo

import other.Tag

fun Tag(a: Int, b: Int): String = "local"

fun box(): String {
    val tag = Tag(1, 2)
    return if (tag.first == 1 && tag.second == 2) "OK" else "FAIL"
}
