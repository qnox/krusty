
open class Base {
    open fun result(): String = "OK"
}

open class Outer : Base() {
    override fun result(): String = "override"

    inner class Inner {
        fun run(): String = super@Outer.result()
    }
}

fun box(): String = Outer().Inner().run()
