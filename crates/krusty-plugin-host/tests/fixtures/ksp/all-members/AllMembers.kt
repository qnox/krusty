package allmembers

interface Shape {
    fun area(): Double
    fun name(): String = "shape"
    val sides: Int
}

abstract class Base {
    open fun describe(): String = ""
    fun fixed() {}
    protected fun prot() {}
    private fun priv() {}
    internal fun inner() {}
    abstract fun abs(): Int
    override fun toString(): String = "Base"
    open val label: String = ""
}

data class Point(val x: Int, val y: Int) : Base(), Shape {
    constructor(x: Int) : this(x, 0)
    override fun area(): Double = 0.0
    override fun abs(): Int = 1
    fun own() {}
    override val sides: Int get() = 0
    fun overload(a: Int) {}
    fun overload(a: String) {}
}

class Plain {
    fun a() {}
}

interface Empty

object Single {
    fun s() {}
}

class Outer {
    inner class Inner {
        fun i() {}
    }
    companion object {
        fun make() {}
    }
}

open class Middle : Base() {
    override fun abs(): Int = 2
}

class Leaf : Middle() {
    override fun describe(): String = "leaf"
}

interface Named {
    fun name(): String = "n"
    fun extra() {}
    val title: String get() = ""
}

data class Extended(val v: Int) : Middle()

interface SubShape : Shape {
    fun more()
    override fun name(): String = "sub"
}

abstract class Impl : SubShape, Named {
    override fun name(): String = "impl"
}

data class Final(val v: Int) : Sealed()

abstract class Sealed {
    final override fun toString(): String = "s"
    final override fun equals(other: Any?): Boolean = false
}

open class Box<T> {
    fun get(): T? = null
    open fun put(value: T) {}
    open val item: T? = null
}

interface Declared {
    override fun toString(): String
}

class Inheriting : Declared

data class Record(val a: Int, var b: String) {
    val extra: Int get() = 1
}

open class Hiding {
    private val hidden = 1
    protected open val shown = 2
    internal var mutable = 3
}

class Showing : Hiding() {
    override val shown = 4
}
