// An array literal copies each spread operand's elements in place, evaluating every operand left
// to right, and never aliases a spread array.
open class Item(val name: String)

private var log = ""

private fun <T> logged(label: String, value: T): T {
    log += label
    return value
}

fun box(): String {
    val head = arrayOf("a", "b")
    val empty = arrayOf<String>()
    val all = arrayOf(logged("1", "x"), *logged("2", head), *empty, logged("3", "y"), *head)
    if (log != "123") return "order $log"
    if (all.size != 6) return "size ${all.size}"
    var joined = ""
    for (value in all) joined += value
    if (joined != "xabyab") return "contents $joined"
    head[0] = "changed"
    if (all[1] != "a") return "aliased"
    val items = arrayOf(Item("i"), Item("j"))
    val copy = arrayOf(*items)
    if (copy === items || copy.size != 2 || copy[1].name != "j") return "copy"
    val nullable = arrayOf<String?>(null, *arrayOf<String?>("n", null))
    if (nullable.size != 3 || nullable[0] != null || nullable[1] != "n" || nullable[2] != null) return "nullable"
    return "OK"
}
