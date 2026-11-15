// A same-package function named like a star-imported class takes a bare call it applies to; when
// it does not apply, the farther class's constructor builds the instance (kotlinc 2.4.21).
// FILE: other.kt
package other

class Tag(val name: String, val count: Int)

// FILE: main.kt
package demo

import other.*

fun Tag(a: Int, b: Int): String = "local"

fun box(): String {
    val local: String = Tag(1, 2)
    val tag: other.Tag = Tag("x", 2)
    return if (local == "local" && tag.name == "x" && tag.count == 2) "OK" else "FAIL: $local"
}
