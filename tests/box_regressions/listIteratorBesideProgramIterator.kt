// A program's own `ListIterator` and an `ArrayList`'s iterator both reach a call through
// `ListIterator`: the program's class answers its own steps back, and the list's iterator answers
// the list's.
class Countdown(private var at: Int) : ListIterator<Int> {
    override fun hasNext(): Boolean = false
    override fun next(): Int = throw NoSuchElementException()
    override fun hasPrevious(): Boolean = at > 0
    override fun previous(): Int = --at
    override fun nextIndex(): Int = at
    override fun previousIndex(): Int = at - 1
}

fun backwards(iterator: ListIterator<Int>): String {
    var text = ""
    while (iterator.hasPrevious()) {
        text += "" + iterator.previousIndex() + ":" + iterator.previous() + " "
    }
    return text + iterator.nextIndex()
}

fun box(): String {
    val list = ArrayList<Int>()
    list.add(4)
    list.add(5)
    val fromList = backwards(list.listIterator(2))
    if (fromList != "1:5 0:4 0") return "FAIL list: $fromList"
    val fromProgram = backwards(Countdown(2))
    if (fromProgram != "1:1 0:0 0") return "FAIL program: $fromProgram"
    return "OK"
}
