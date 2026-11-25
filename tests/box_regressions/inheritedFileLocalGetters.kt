package fixture

abstract class Base {
    protected var state: Int = 3
    protected var failure: Throwable? = null
    protected var result: Any? = "r"
    protected val seed: Int get() = 7
}

open class Level {
    protected open val level: Int get() = 7
}

open class Refined : Level() {
    protected override val level: Int get() = 21
}

abstract class Middle : Base() {
    protected val extra: Int get() = seed * 3
}

fun readInherited(): String {
    val o = object : Base() {
        fun run(): String = "$result/$failure/$state/$seed"
    }
    return o.run()
}

fun readThroughIntermediate(): String {
    val o = object : Middle() {
        fun run(): String = "$result/$state/$seed/$extra"
    }
    return o.run()
}

fun readDeclared(): Int {
    val o = object {
        val own: Int get() = 5
        fun read() = own
    }
    return o.read()
}

fun readDeclaredAndInherited(): String {
    val o = object : Middle() {
        val own: Int get() = 5
        fun run(): String = "${own + seed + extra}"
    }
    return o.run()
}

fun readOverridden(): String {
    val o = object : Refined() {
        fun run(): String = "$level"
    }
    return o.run()
}

fun box(): String {
    if (readInherited() != "r/null/3/7") return "inherited ${readInherited()}"
    if (readThroughIntermediate() != "r/3/7/21") return "through an intermediate class ${readThroughIntermediate()}"
    if (readDeclared() != 5) return "declared ${readDeclared()}"
    if (readDeclaredAndInherited() != "33") return "declared and inherited ${readDeclaredAndInherited()}"
    if (readOverridden() != "21") return "overridden ${readOverridden()}"
    return "OK"
}
