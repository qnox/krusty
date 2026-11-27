// A library that declares no package publishes its top-level declarations in the root package.
// The consumer calls a top-level function, reads a top-level property, and constructs a class and
// calls its member, each lowered from the library's own body.
// MODULE: lib
fun greeting(): String = "O" + suffix

val suffix: String = "K"

class Counter(private val start: Int) {
    fun next(): Int = start + 1
}

// MODULE: main(lib)
fun box(): String {
    if (Counter(1).next() != 2) return "FAIL counter"
    return greeting()
}
