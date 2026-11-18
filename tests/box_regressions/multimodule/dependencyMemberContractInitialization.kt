// WITH_STDLIB
// MODULE: lib
// FILE: Lib.kt
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
package neutral

import kotlin.contracts.*

object Source {
    inline fun fill(size: Int, action: (ByteArray, Int, Int) -> Int): Int {
        contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }
        return action(ByteArray(size), 0, size)
    }
}

class Holder {
    inline fun once(action: () -> Int): Int {
        contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }
        return action()
    }
}
// MODULE: main(lib)
// FILE: Main.kt
import neutral.Holder
import neutral.Source

fun fromObject(): Int {
    var result: Int
    Source.fill(3) { _, _, end ->
        result = end
        result
    }
    return result
}

fun fromInstance(): Int {
    val result: Int
    Holder().once {
        result = 4
        result
    }
    return result
}

fun box(): String = if (fromObject() == 3 && fromInstance() == 4) "OK" else "FAIL"
