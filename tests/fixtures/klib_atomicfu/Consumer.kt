package fixture

fun box(): String {
    val counter = Counter()
    if (counter.add(2) != 2) return "fail: add 2"
    if (counter.add(3) != 5) return "fail: add 3"
    if (counter.next() != 6) return "fail: next"
    if (counter.rename("a") != null) return "fail: rename a"
    if (counter.rename("b") != "a") return "fail: rename b"
    val holder = Holder()
    if (holder.handle != null) return "fail: initial handle"
    holder.handle = object : Handle {
        override fun id(): Int = 7
    }
    if (holder.handle?.id() != 7) return "fail: handle ${holder.handle?.id()}"
    return "OK"
}
