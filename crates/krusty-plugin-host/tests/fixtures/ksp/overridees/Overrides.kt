package overrides

open class A {
    open fun f(): Int = 0
    open fun g(x: Int): Int = x
    open val p: Int = 0
    override fun toString(): String = "A"
}

open class B : A() {
    override fun g(x: Int): Int = x + 1
}

class C : B() {
    override fun f(): Int = 1
    override fun g(x: Int): Int = x + 2
    override val p: Int = 1
    override fun hashCode(): Int = 0
}

interface I {
    fun f(): Int
    val q: Int
}

interface J {
    fun f(): Int = 0
}

abstract class D : A(), I {
    override fun f(): Int = 2
    override val q: Int = 0
}

class E : I, J {
    override fun f(): Int = 3
    override val q: Int get() = 0
}

data class G(val v: Int) {
    override fun toString(): String = ""
}

open class Box<T> {
    open fun put(item: T): Unit {}
    open val content: T? get() = null
}

class StringBox : Box<String>() {
    override fun put(item: String): Unit {}
    override val content: String? get() = null
}

abstract class Over : A() {
    abstract override fun g(x: Int): Int
}
