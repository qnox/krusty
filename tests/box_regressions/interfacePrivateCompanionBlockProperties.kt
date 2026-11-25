// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
// FILE: Secrets.kt
package secrets

interface Secrets {
    companion {
        private val greeting = "O"
        private var secret = "x"
        internal inline fun readSecret() = secret
        internal inline fun writeSecret(value: String) {
            secret = value
        }
    }

    class Reader {
        fun read() = greeting
    }

    companion object {
        fun read() = greeting
    }
}

// FILE: Main.kt
import secrets.Secrets

fun box(): String {
    if (Secrets.Reader().read() != "O") return "fail nested class"
    if (Secrets.read() != "O") return "fail companion object"
    Secrets.writeSecret("K")
    return Secrets.Reader().read() + Secrets.readSecret()
}
