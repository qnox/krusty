// WITH_STDLIB

fun box(): String {
    val list = ArrayList<String>()
    list.add("a")
    list.add("b")
    val listIterator: Any = list.listIterator()
    if (listIterator !is ListIterator<*>) return "FAIL listIterator is ListIterator"
    if (listIterator !is Iterator<*>) return "FAIL listIterator is Iterator"
    val atIndex: Any = list.listIterator(1)
    if (atIndex !is ListIterator<*>) return "FAIL listIterator(index) is ListIterator"
    val cast = listIterator as ListIterator<*>
    if (cast.next() != "a") return "FAIL cast iterator walks"
    if (list as Any is ListIterator<*>) return "FAIL list is ListIterator"
    return "OK"
}
