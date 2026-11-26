// The number of zero bits above the highest set one, at the receiver's own width.
fun box(): String {
    if (0.countLeadingZeroBits() != 32) return "Fail int zero"
    if (1.countLeadingZeroBits() != 31) return "Fail int one"
    if (255.countLeadingZeroBits() != 24) return "Fail int byte"
    if (Int.MAX_VALUE.countLeadingZeroBits() != 1) return "Fail int max"
    if (Int.MIN_VALUE.countLeadingZeroBits() != 0) return "Fail int min"
    if ((-1).countLeadingZeroBits() != 0) return "Fail int minus one"
    if (0L.countLeadingZeroBits() != 64) return "Fail long zero"
    if (1L.countLeadingZeroBits() != 63) return "Fail long one"
    if ((1L shl 32).countLeadingZeroBits() != 31) return "Fail long high word"
    if (Long.MAX_VALUE.countLeadingZeroBits() != 1) return "Fail long max"
    if (Long.MIN_VALUE.countLeadingZeroBits() != 0) return "Fail long min"
    return "OK"
}
