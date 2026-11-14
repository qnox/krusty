// FILE: base.kt
open class Base(val value: String) {
    var marker = 0

    constructor(value: String, marker: Int) : this(value) {
        this.marker = marker
    }

    open fun render(): String = "$value:$marker"
}

// FILE: child.kt
class Child : Base {
    constructor(value: String) : super(value, 7)
}

fun box(): String {
    val child = Child("ready")
    if (child.render() != "ready:7") return "secondary ${child.render()}"
    return "OK"
}
