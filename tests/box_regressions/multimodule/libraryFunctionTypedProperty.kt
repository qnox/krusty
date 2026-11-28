// A library class stores a function-typed constructor property. The KLIB writer has no IR type
// for a function type yet, so it keeps the file's bodies unwritten instead of failing, and a
// program linked against the KLIB reaches the class through the library's metadata.
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
