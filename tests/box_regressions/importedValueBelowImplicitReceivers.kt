// An implicit receiver's member outranks the enum entries star imports offer, even when two star
// imports offer the same name and would be ambiguous between themselves.

// FILE: second/SomeEnum.kt
package second

enum class SomeEnum { SOME; }

// FILE: third/SomeEnum.kt
package third

enum class SomeEnum { SOME; }

// FILE: main.kt
import second.SomeEnum.*
import third.SomeEnum.*

class Holder {
    val SOME: String = "member"
}

fun Holder.read(): String = SOME

class Inside {
    val SOME: String = "inside"

    fun read(): String = SOME
}

fun box(): String {
    val extension = Holder().read()
    val member = Inside().read()
    return if (extension == "member" && member == "inside") "OK" else "fail: $extension $member"
}
