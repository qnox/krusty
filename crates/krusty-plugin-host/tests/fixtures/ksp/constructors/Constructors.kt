package constructors

class Implicit

class Explicit(first: Int, second: String = "")

class SecondaryOnly {
    constructor(value: Int)
}

class Both(val a: Int, var b: String = "", vararg c: Int) {
    constructor() : this(1)
}

object Single

annotation class Marker(val name: String)

interface Shape
