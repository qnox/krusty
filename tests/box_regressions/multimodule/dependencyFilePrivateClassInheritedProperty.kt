// MODULE: lib
// FILE: Nodes.kt
package fixture

// A library body reads and writes a property that a class private to its file inherits: kotlinc
// names the inherited accessors by the identity they would have in a public class, scoped to the
// file, and no declaration of the library defines them.
abstract class Node {
    var mark: Int = 0
}

private class Leaf(val value: Int) : Node()

fun markedLeaf(value: Int): Int {
    val leaf = Leaf(value)
    leaf.mark = leaf.mark + 5
    return leaf.mark + leaf.value
}

// MODULE: main(lib)
// FILE: main.kt
import fixture.markedLeaf

fun box(): String {
    val marked = markedLeaf(2)
    return if (marked == 7) "OK" else "FAIL $marked"
}
