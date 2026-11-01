package overridees

class Greeting(val name: String, var count: Int = 1) {
    val length: Int get() = name.length
    fun greet(): String = name
}

open class Base {
    open fun hook() {}
    open val label: String = ""
}

interface Shape {
    fun area(): Double
    val sides: Int
}

object Single {
    fun objectMember() {}
}

fun topLevel() {}
val topLevelProperty = 1
