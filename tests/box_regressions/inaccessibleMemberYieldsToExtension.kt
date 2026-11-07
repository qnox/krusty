// A member the call site cannot access does not hide an accessible extension of the same name:
// kotlinc ranks a visibility error below every resolved candidate, on any tower rung.

open class Base {
    protected fun tag(): String = "protected member"
}

class Item(val name: String) : Base() {
    private fun label(): String = "private member"
}

fun Item.label(): String = "top-level extension " + name

class Holder {
    private fun Item.tag(): String = "member extension " + name

    fun read(item: Item): String = item.tag()

    fun readSafe(item: Item?): String? = item?.tag()
}

fun box(): String {
    val item = Item("a")
    if (item.label() != "top-level extension a") return "Fail top-level: " + item.label()
    val holder = Holder()
    if (holder.read(item) != "member extension a") return "Fail member: " + holder.read(item)
    if (holder.readSafe(item) != "member extension a") return "Fail safe: " + holder.readSafe(item)
    if (holder.readSafe(null) != null) return "Fail null"
    return "OK"
}
