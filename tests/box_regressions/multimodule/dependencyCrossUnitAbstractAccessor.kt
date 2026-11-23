// MODULE: lib
// FILE: Sized.kt
package fixture

abstract class Sized {
    abstract val size: Int
}

class Three : Sized() {
    override val size: Int = 3
}

// A library body that reads the abstract accessor.
fun sizeOf(sized: Sized): Int = sized.size

// MODULE: main(lib)
// FILE: First.kt
package fixture

// The first checked source realizes `Sized` and `Three`.
fun firstSize(): Int = Three().size

// FILE: Consumer.kt
package fixture

// The later one lowers `sizeOf`, whose body reads an accessor of the class the first source
// realized: the read dispatches through that class's declaration.
fun box(): String {
    if (firstSize() != 3) return "first:${firstSize()}"
    val size = sizeOf(Three())
    return if (size == 3) "OK" else "second:$size"
}
