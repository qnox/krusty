// The same first use as classCompanionBlockFirstUse.kt, with the using file first.
// LANGUAGE: +CompanionBlocksAndExtensions
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

// FILE: Secrets.kt
package secrets

class Secrets {
    companion {
        var secret = "O"
        val greeting = "K"
    }
}
