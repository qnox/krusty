fun pick(flag: Boolean): UInt {
    val value: UInt
    if (flag) value = 5u else value = 6u
    return value
}

fun wide(flag: Boolean): ULong {
    val value: ULong
    value = if (flag) 7uL else 8uL
    return value
}

fun box(): String {
    if (pick(true) != 5u) return "fail: UInt then"
    if (pick(false) != 6u) return "fail: UInt else"
    if (wide(true) != 7uL) return "fail: ULong then"
    if (wide(false) != 8uL) return "fail: ULong else"
    return "OK"
}
