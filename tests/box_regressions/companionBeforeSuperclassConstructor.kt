// Initializing a class initializes its superclass first. With both companions present the
// superclass companion runs before the subclass companion.

var log = ""

open class Base {
    companion object { init { log += "B" } }
}

class Derived : Base() {
    companion object { init { log += "D" } }
}

fun box(): String {
    Derived()
    return if (log == "BD") "OK" else log
}
