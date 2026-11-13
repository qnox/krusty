// A local class's `@LowPriorityInOverloadResolution` constructor yields to the implicit
// receiver's member of the same name, though the local scope comes first, while an ordinary
// local constructor wins over it.
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

class Scope {
    fun Item(v: String): String = "member $v"

    fun Plain(v: String): String = "member $v"

    fun run(): String {
        class Item @kotlin.internal.LowPriorityInOverloadResolution constructor(val v: String)
        class Plain(val v: String)
        val low: Any = Item("x")
        val plain: Any = Plain("y")
        return if (low == "member x" && plain is Plain) "OK" else "low $low, plain $plain"
    }
}

fun box(): String = Scope().run()
