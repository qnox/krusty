// A class's companion-block storage is initialized by the first use of the class, even when
// that use is a read from another file that runs before the declaring file's statics.
// LANGUAGE: +CompanionBlocksAndExtensions
// FILE: Secrets.kt
package secrets

class Secrets {
    companion {
        var secret = "O"
        val greeting = "K"
    }
}

// FILE: Main.kt
import secrets.Secrets

fun box(): String {
    // The first use of the class is a read of its block storage from another file.
    val first = Secrets.secret
    if (first != "O") return "fail first read: $first"
    Secrets.secret = "x"
    if (Secrets.secret != "x") return "fail write"
    return first + Secrets.greeting
}
