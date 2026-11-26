// An enum whose `entries` nothing reads still carries a list of its own on Kotlin/Wasm; a program
// with a property reference to its members must not pull the stdlib's list classes in for it.
enum class Level {
    LOW, HIGH
}

fun box(): String {
    val name = (Level::name).get(Level.HIGH)
    if (name != "HIGH") return "Fail name $name"
    val ordinal = (Level::ordinal).get(Level.HIGH)
    if (ordinal != 1) return "Fail ordinal $ordinal"
    return "OK"
}
