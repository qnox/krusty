// FILE: user.kt
// A file that references a property declared in a later file reads it through the same checked
// property, and its references equal the declaring file's.
fun fromUser(): (Later) -> Int = Later::y

fun box(): String {
    val local: (Later) -> Int = Later::y
    if (fromUser() != local) return "user"
    if (fromUser() != fromLater()) return "later"
    if (fromUser()(later(5)) != 5 || fromLater()(later(6)) != 6) return "invoke"
    return "OK"
}

// FILE: later.kt
class Later(val y: Int)

fun fromLater(): (Later) -> Int = Later::y

fun later(y: Int): Later = Later(y)
