@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")

class Cell(val name: String)

class Secret {
    private fun hidden(): String = "h"
}

// The suppression still lets a call whose only candidate is the invisible member resolve to it.
fun neverCalled(secret: Secret): String = secret.hidden()

class Holder {
    private fun <T : Any> T.clone(suffix: String = "!"): T = this

    fun copied(cell: Cell): String = cell.clone().name

    fun <T : Any> same(value: T): T = value.clone()
}

fun box(): String {
    val holder = Holder()
    if (holder.copied(Cell("a")) != "a") return "member"
    if (holder.same("s") != "s") return "generic"
    return "OK"
}
