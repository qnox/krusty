fun same(x: Int?) = x?.toByte()?.hashCode()?.equals(x)

fun box(): String {
    if (same(42) != true) return "fail present"
    if (same(null) != null) return "fail absent"
    return "OK"
}
