// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
// FILE: Main.kt
import secrets.Secrets

fun box(): String {
    // The first use of the interface is an inline read of its private storage from another file.
    val first = Secrets.readSecret()
    if (first != "O") return "fail first read: $first"
    Secrets.writeSecret("x")
    if (Secrets.readSecret() != "x") return "fail write"
    return first + Secrets.readGreeting()
}

// FILE: Secrets.kt
package secrets

interface Secrets {
    companion {
        private var secret = "O"
        private val greeting = "K"
        internal inline fun readSecret() = secret
        internal inline fun writeSecret(value: String) {
            secret = value
        }
        internal inline fun readGreeting() = greeting
    }
}
