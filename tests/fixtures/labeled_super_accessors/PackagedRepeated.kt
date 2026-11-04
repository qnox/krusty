package bridge.names

class Namespace {
    open class `Base$Part` {
        open fun result(): String = "O"
        open var value: String = "K"
    }

    class Outer : `Base$Part`() {
        override fun result(): String = "override"
        override var value: String = "override"

        inner class Inner {
            fun run(): String {
                val first = super@Outer.result()
                val second = super@Outer.result()
                val before = super@Outer.value
                super@Outer.value = "!"
                val after = super@Outer.value
                super@Outer.value = "K"
                val restored = super@Outer.value
                return first + second + before + after + restored
            }
        }
    }
}

fun box(): String {
    val result = Namespace.Outer().Inner().run()
    return if (result == "OOK!K") "OK" else result
}
