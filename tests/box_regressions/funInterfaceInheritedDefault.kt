interface Parent {
    fun visit(visitor: (Parent) -> Unit) {
        visitor.invoke(this)
    }

    fun twice(value: Int): Int = value * 2
}

fun interface Child : Parent {
    fun intValue(): Int
}

fun box(): String {
    var result = "fail"
    val child = Child { 21 }
    child.visit { result = "OK" }
    if (child.twice(child.intValue()) != 42) return "Fail twice"
    val parent: Parent = child
    if (parent.twice(4) != 8) return "Fail through the parent"
    return result
}
