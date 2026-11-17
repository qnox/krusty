// An explicitly imported enum entry outranks a same-named entry a star import offers.

// FILE: second/SomeEnum.kt
package second

enum class SomeEnum { SOME; }

// FILE: third/SomeEnum.kt
package third

enum class SomeEnum { SOME; }

// FILE: main.kt
import second.SomeEnum.SOME
import third.SomeEnum.*

fun box(): String = if (SOME == second.SomeEnum.SOME) "OK" else "fail"
