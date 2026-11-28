// A library's interfaces, abstract classes and objects, and the classes that initialize body
// properties and run `init` blocks, are written into its KLIB. A program linked against that KLIB
// dispatches through the interface and abstract members and reads the object's state.
// MODULE: lib
// FILE: Library.kt
package fixture

interface Shape {
    fun area(): Int

    fun describe(): String = "shape ${area()}"
}

abstract class Named(val name: String) : Shape {
    abstract fun kind(): String

    var visits: Int = 0

    init {
        visits = visits + 1
    }

    fun label(): String = "$name:${kind()}"
}

class Square(val side: Int) : Named("square") {
    override fun area(): Int = side * side

    override fun kind(): String = "square"

    val doubled: Int = side * 2

    init {
        visits = visits + doubled
    }
}

object Registry {
    var count: Int = 0

    fun register(): Int {
        count = count + 1
        return count
    }
}

fun sides(shape: Shape): Int = shape.area()

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    val square = Square(3)
    if (square.area() != 9) return "area"
    if (square.describe() != "shape 9") return "describe"
    if (square.label() != "square:square") return "label"
    if (square.doubled != 6) return "doubled"
    if (square.visits != 7) return "init"
    val shape: Shape = square
    if (sides(shape) != 9) return "interface"
    val named: Named = square
    if (named.kind() != "square") return "abstract"
    if (Registry.register() != 1 || Registry.register() != 2) return "object"
    if (Registry.count != 2) return "object state"
    return "OK"
}
