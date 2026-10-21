// MODULE: lib
fun inlineBehavior(): String = buildString { append("O"); append('K') }
// MODULE: main(lib)
fun box(): String = inlineBehavior()
