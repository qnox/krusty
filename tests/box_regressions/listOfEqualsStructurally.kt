fun a() = listOf("e")
fun box(): String {
    if (listOf("e") != listOf("e")) return "fail1"
    if (a() != listOf("e")) return "fail2"
    val x: List<String> = listOf("e")
    if (!x.equals(a())) return "fail3"
    if (listOf("e") == listOf("f")) return "fail4"
    if (listOf("e") == listOf("e", "f")) return "fail5"
    val built = ArrayList<String>()
    built.add("e")
    if (built != a()) return "fail6"
    if (a() != built) return "fail7"
    return "OK"
}
