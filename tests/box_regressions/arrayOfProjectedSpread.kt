// A spread argument of `arrayOf`'s `vararg T` accepts any `Array<out T>`, as for other vararg
// calls: spreading an `Array<out Item>` infers `T = Item`.
open class Item(val name: String)
class Sub(name: String) : Item(name)

fun pick(flag: Boolean): Array<out Item> = if (flag) arrayOf(Sub("a"), Sub("b")) else arrayOf(Item("c"))

fun inferred(flag: Boolean): Int {
    val projected = pick(flag)
    val all = arrayOf(*projected, Item("s"))
    return all.size
}

fun explicit(): String {
    val projected: Array<out Item> = arrayOf(Sub("d"))
    val all = arrayOf<Item>(*projected, Item("t"))
    all[0] = Item("replaced")
    return all[0].name + all[1].name
}

fun expected(): Array<Any> {
    val projected: Array<out String> = arrayOf("u")
    return arrayOf(*projected, Item("v"))
}

fun strings(): String {
    val projected: Array<out String> = arrayOf("w", "x")
    val all = arrayOf(*projected, *projected)
    return all[0] + all[3]
}

fun box(): String {
    if (inferred(true) != 3 || inferred(false) != 2) return "inferred"
    if (explicit() != "replacedt") return "explicit"
    val e = expected()
    if (e.size != 2 || e[0] != "u" || (e[1] as Item).name != "v") return "expected"
    if (strings() != "wx") return "strings"
    return "OK"
}
