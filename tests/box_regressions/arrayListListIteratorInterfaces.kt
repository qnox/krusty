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
    if (cast.hasPrevious()) return "FAIL fresh iterator has a previous element"
    if (cast.nextIndex() != 0) return "FAIL fresh nextIndex: ${cast.nextIndex()}"
    if (cast.previousIndex() != -1) return "FAIL fresh previousIndex: ${cast.previousIndex()}"
    if (cast.next() != "a") return "FAIL cast iterator walks"
    if (!cast.hasPrevious()) return "FAIL no previous element after next"
    if (cast.nextIndex() != 1) return "FAIL nextIndex after next: ${cast.nextIndex()}"
    if (cast.previousIndex() != 0) return "FAIL previousIndex after next: ${cast.previousIndex()}"
    if (cast.previous() != "a") return "FAIL previous steps back"
    val fromIndex = atIndex as ListIterator<*>
    if (fromIndex.nextIndex() != 1) return "FAIL listIterator(1) nextIndex: ${fromIndex.nextIndex()}"
    if (fromIndex.previous() != "a") return "FAIL listIterator(1) previous"
    if (fromIndex.next() != "a" || fromIndex.next() != "b") return "FAIL listIterator(1) walks forward"
    if (fromIndex.hasNext()) return "FAIL walked iterator has a next element"
    if (list as Any is ListIterator<*>) return "FAIL list is ListIterator"
    return "OK"
}
