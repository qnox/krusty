// LANGUAGE: +ContextParameters
// MODULE: lib
class Prefix(val value: String)
context(prefix: Prefix) fun read() = prefix.value
fun inlineBehavior(): String {
    context(Prefix("O")) {
        if (read() != "O") return "fail"
    }
    return "OK"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
