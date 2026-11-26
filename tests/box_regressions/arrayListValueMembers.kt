fun box(): String {
    val a = ArrayList<Int>()
    a.add(7)
    a.add(8)
    val b = ArrayList<Int>()
    b.add(7)
    b.add(8)
    if (a != b) return "equals copy"
    b.add(9)
    if (a == b) return "length"
    if (a.hashCode() != 31 * (31 * 1 + 7) + 8) return "hash value"
    if (a.hashCode() == b.hashCode()) return "hash of different lists"
    if (a.toString() != "[7, 8]") return "string " + a.toString()
    if ("$a" != "[7, 8]") return "template"
    if (ArrayList<Int>().toString() != "[]") return "empty string"
    if (ArrayList<Int>() != ArrayList<Int>()) return "empty equals"
    if (ArrayList<Int>().hashCode() != 1) return "empty hash"
    val names = ArrayList<String?>()
    names.add("x")
    names.add(null)
    if (names.toString() != "[x, null]") return "nullable string " + names.toString()
    val nested = ArrayList<Any?>()
    nested.add(a)
    if (nested.toString() != "[[7, 8]]") return "nested " + nested.toString()
    val self = ArrayList<Any?>()
    self.add(1)
    self.add(self)
    if (self.toString() != "[1, (this Collection)]") return "self " + self.toString()
    return "OK"
}
