// Open stdlib collection classes built from their library bodies on a target that lays out none
// of them: `HashMap`, `LinkedHashMap` and `HashSet` are open classes whose members dispatch.
fun box(): String {
    val m = HashMap<String, String>()
    m["a"] = "O"
    m["b"] = "K"
    if (m.size != 2) return "Fail size: ${m.size}"
    if (m["c"] != null) return "Fail absent"
    if (!m.containsKey("a") || m.containsKey("z")) return "Fail containsKey"
    if (m.remove("b") != "K" || m.size != 1) return "Fail remove"

    val linked = LinkedHashMap<String, Int>()
    linked["x"] = 1
    linked["y"] = 2
    var order = ""
    for (entry in linked) order += entry.key + entry.value
    if (order != "x1y2") return "Fail order: $order"

    val set = HashSet<String>()
    set.add("p")
    set.add("p")
    set.add("q")
    if (set.size != 2 || !set.contains("q") || set.contains("r")) return "Fail set"
    return m["a"]!! + "K"
}
