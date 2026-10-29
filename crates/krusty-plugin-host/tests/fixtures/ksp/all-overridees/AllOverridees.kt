package alloverridees

data class Point(val x: Int)

data object Origin

class Plain

open class Named {
    override fun toString(): String = "named"
}

data class Label(val text: String) : Named()
