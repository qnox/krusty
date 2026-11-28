// LANGUAGE: +CompanionBlocks +CompanionExtensions

// A companion block member of an inner class of a generic class takes no value parameter for the
// class it is associated with, and a companion extension's receiver is that bare classifier.
class Outer<T> {
    inner class Inner {
        companion {
            fun static(): String = "OK"
        }
    }
}

companion fun Outer.Inner.extension(): String = "ext"

fun box(): String {
    if (Outer.Inner.extension() != "ext") return "Fail: extension"
    return Outer.Inner.static()
}
