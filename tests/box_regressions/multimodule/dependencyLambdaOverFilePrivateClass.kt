// MODULE: lib
// FILE: Handles.kt
package fixture

// A library lambda takes a value of a class private to its file, as `also` on a fresh instance of
// that class does: the lambda's parameter names the private class.
interface Handle {
    fun dispose(): Int
}

private class Counted(var value: Int) : Handle {
    override fun dispose(): Int = value + 1
}

fun attach(value: Int): Handle = Counted(value).also { it.value += 1 }

// MODULE: main(lib)
// FILE: main.kt
import fixture.attach

fun box(): String {
    val disposed = attach(1).dispose()
    return if (disposed == 3) "OK" else "FAIL $disposed"
}
