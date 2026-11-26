// LANGUAGE: +ContextParameters
context(c: C)
fun <T, C : () -> T> read(x: T): T = c()

fun box(): String {
    val supply: () -> String = { "OK" }
    return context(supply) {
        context(42) {
            read("")
        }
    }
}
