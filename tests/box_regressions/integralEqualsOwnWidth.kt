fun box(): String {
    val b: Byte = 7
    val s: Short = 300
    val i = 70000
    val l = 5000000000L
    if (!b.equals(7.toByte()) || b.equals(8.toByte())) return "fail byte"
    if (!s.equals(300.toShort()) || s.equals(301.toShort())) return "fail short"
    if (!i.equals(70000) || i.equals(70001)) return "fail int"
    if (!l.equals(5000000000L) || l.equals(5000000001L)) return "fail long"
    return "OK"
}
