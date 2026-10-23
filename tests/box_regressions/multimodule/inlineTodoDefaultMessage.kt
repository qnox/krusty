// MODULE: lib
fun inlineBehavior(): String = try {
    TODO()
} catch (e: NotImplementedError) {
    if (e.message == "An operation is not implemented.") "OK" else "fail: ${e.message}"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
