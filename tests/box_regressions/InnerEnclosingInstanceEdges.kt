class EdgeOwner(val value: String) {
    inner class Middle {
        inner class Leaf {
            fun read(): String = value
        }
    }
}

class GenericEdge<T>(val value: T) {
    inner class Inner {
        fun read(): T = value
    }
}

private var enclosingEffects = 0

private fun nextLeaf(owner: EdgeOwner): EdgeOwner.Middle.Leaf {
    enclosingEffects++
    return owner.Middle().Leaf()
}

fun box(): String {
    val first = nextLeaf(EdgeOwner("O")).read()
    if (enclosingEffects != 1) return "FAIL effects"
    return first + GenericEdge("K").Inner().read()
}
