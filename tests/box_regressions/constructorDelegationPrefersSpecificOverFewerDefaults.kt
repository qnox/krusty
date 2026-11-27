// Fewer omitted defaults wins only between equally specific constructors. `this(1)` from the
// `Long` constructor selects `(x: Int, y: Int = 2)`, the more specific one for an integer literal,
// and is not a call of the delegating constructor itself.

class Primary(val x: Int = 1, val y: Int = 2) {
    var tag = "primary"

    constructor(z: Long) : this(1) {
        tag = "long$z"
    }
}

class Secondary(val s: String) {
    constructor(x: Int, y: Int = 2) : this("int$x$y")

    constructor(z: Long) : this("long$z")
}

fun pick(x: Int, y: Int = 2) = "int$x$y"

fun pick(z: Long) = "long$z"

fun box(): String {
    val primary = Primary(5L)
    if (primary.tag != "long5" || primary.x != 1 || primary.y != 2) return "primary: ${primary.tag}"
    if (Secondary(1).s != "int12") return "secondary: ${Secondary(1).s}"
    if (Secondary(3L).s != "long3") return "secondary long: ${Secondary(3L).s}"
    if (pick(1) != "int12") return "call: ${pick(1)}"
    return "OK"
}
