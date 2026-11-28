fun box(): String {
    val d: Double = -1.5
    if (d.toUInt() != 0u) return "fail: negative double to UInt"
    val big: Double = 1.0e20
    if (big.toUInt() != UInt.MAX_VALUE) return "fail: large double to UInt"
    return "OK"
}
