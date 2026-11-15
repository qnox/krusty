// WITH_STDLIB
// FILE: Library.kt
package library

fun test(): String = "O"

inline fun String.switchMapOnce(crossinline mapper: (String) -> String): String {
    val reference: () -> String = ::test
    return reference() + { mapper(this) }()
}
// FILE: Caller.kt
package caller

import library.*

fun box(): String {
    val result = "A".switchMapOnce {
        "B".switchMapOnce { it + "K" }
    }
    return if (result == "OOBK") "OK" else result
}
