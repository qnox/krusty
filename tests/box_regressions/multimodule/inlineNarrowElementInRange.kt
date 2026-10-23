// MODULE: lib
fun inlineBehavior(): String {
    val range = 1..3
    val inside: Byte = 2
    val outside: Byte = 4
    return if (inside in range && outside !in range) "OK" else "fail"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
