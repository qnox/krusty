// A same-package or star-imported function named like a default-imported class takes a bare call
// ahead of that class's constructor when it applies (kotlinc 2.4.21).
// FILE: other.kt
package other

fun Triple(a: Int, b: Int, c: Int): String = "star"

// FILE: main.kt
package demo

import other.*

fun Pair(a: Int, b: Int): String = "local"

fun box(): String {
    val local: String = Pair(1, 2)
    if (local != "local") return "FAIL: $local"
    val star: String = Triple(1, 2, 3)
    if (star != "star") return "FAIL: $star"
    return "OK"
}
