// A library class stores a function-typed constructor property, which a member invokes and a
// top-level function fills with a lambda. The KLIB records the property's function type and the
// bodies that build and call it, and the program runs the library's code from its KLIB.
// MODULE: lib
// FILE: Library.kt
package fixture

class Holder(val callback: () -> String) {
    fun run(): String = callback()
}

fun holder(): Holder = Holder { "OK" }

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String = holder().run()
