
open class Base(val saved: String)
private fun copy(value: String) = object : Base(value) {}
fun box(): String = copy("OK").saved
