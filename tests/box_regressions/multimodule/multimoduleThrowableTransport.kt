// MODULE: lib
fun raiseHeld(value: Throwable): Nothing = throw value
fun relayHeld(value: Throwable): Nothing {
    try { raiseHeld(value) } catch (caught: Throwable) { throw caught }
}
// MODULE: main(lib)
fun box(): String {
    val held = Throwable("held")
    val caught = try { relayHeld(held) } catch (value: Throwable) { value }
    if (caught !== held) return "identity"
    if (caught.message != "held") return "message"
    val empty = Throwable()
    if (empty.message != null) return "empty message"
    try { raiseHeld(empty) } catch (value: Throwable) {
        if (value !== empty) return "empty identity"
    }
    return "OK"
}
