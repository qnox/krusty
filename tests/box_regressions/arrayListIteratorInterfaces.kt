// WITH_STDLIB

fun box(): String {
    val list = ArrayList<String>()
    list.add("a")
    list.add("b")
    val plain: Any = list.iterator()
    if (plain !is Iterator<*>) return "FAIL iterator is Iterator"
    val cast = plain as Iterator<*>
    if (cast.next() != "a") return "FAIL cast iterator walks"
    if (list as Any is Iterator<*>) return "FAIL list is Iterator"
    return "OK"
}
