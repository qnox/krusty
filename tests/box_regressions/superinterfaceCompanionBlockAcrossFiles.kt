// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
// FILE: Greeter.kt
package greet

var log = ""

fun record(step: String): String {
    log += step
    return step
}

// Declares a non-abstract member, so initializing an implementing class initializes it first.
interface Greeter {
    companion {
        val greeting = record("I")
    }

    fun greet(): String = "hi"
}

// Declares only abstract members, so an implementing class leaves it alone.
interface Silent {
    companion {
        val quiet = record("S")
    }

    fun speak(): String
}

// FILE: Main.kt
import greet.*

class Polite : Greeter, Silent {
    companion {
        val manners = record("C")
    }

    override fun speak(): String = "..."
}

fun box(): String {
    // The first use of the class is its construction in another file than its superinterfaces.
    Polite()
    if (log != "IC") return "fail construction: $log"
    val values = Silent.quiet + Polite.manners + Greeter.greeting
    if (values != "SCI") return "fail values: $values"
    if (log != "ICS") return "fail first use of Silent: $log"
    return "OK"
}
