@file:OptIn(ExperimentalUnsignedTypes::class)

fun <T> keepUnsignedView(value: T): T = value

fun checkUnsignedViews(first: Any, second: Any, separate: Any, signed: Any, hash: Int): String {
    if (first != second || second != first) return "same storage differs"
    if (first == separate || separate == first) return "different storage equals"
    if (first == signed || signed == first) return "signed array equals unsigned view"
    if (first.hashCode() != hash || second.hashCode() != hash) return "signed content hash"
    val stored = arrayOf(first, second)
    if (stored[0] != stored[1]) return "reference array loses view"
    if (keepUnsignedView(first) != second) return "generic call loses view"
    return "OK"
}

fun box(): String {
    val bytes = byteArrayOf(-1, 2)
    val ub = bytes.asUByteArray()
    val ubAgain = bytes.asUByteArray()
    if (ub.asByteArray() !== bytes || ubAgain.asByteArray() !== bytes) return "byte backing identity"
    val boxedByte: Any = keepUnsignedView(ub)
    if (boxedByte !is UByteArray || boxedByte is ByteArray) return "byte nominal identity"
    val byteCheck = checkUnsignedViews(ub, ubAgain, ubyteArrayOf(255u, 2u), bytes, 932)
    if (byteCheck != "OK") return "byte: $byteCheck"
    ub[1] = 3u
    if (bytes[1] != 3.toByte() || ubAgain[1] != 3.toUByte()) return "byte write through view"
    bytes[0] = 1
    if (ub[0] != 1.toUByte() || (ub as Any).hashCode() != 995) return "byte write through backing"

    val shorts = shortArrayOf(-1, 2)
    val us = shorts.asUShortArray()
    val usAgain = shorts.asUShortArray()
    if (us.asShortArray() !== shorts || usAgain.asShortArray() !== shorts) return "short backing identity"
    val boxedShort: Any = keepUnsignedView(us)
    if (boxedShort !is UShortArray || boxedShort is ShortArray) return "short nominal identity"
    if (boxedByte == boxedShort) return "different unsigned widths equal"
    val shortCheck = checkUnsignedViews(us, usAgain, ushortArrayOf(65535u, 2u), shorts, 932)
    if (shortCheck != "OK") return "short: $shortCheck"
    us[1] = 3u
    shorts[0] = 1
    if (usAgain[1] != 3.toUShort() || us[0] != 1.toUShort()) return "short alias writes"
    if ((us as Any).hashCode() != 995) return "short hash after mutation"

    val ints = intArrayOf(-1, 2)
    val ui = ints.asUIntArray()
    val uiAgain = ints.asUIntArray()
    if (ui.asIntArray() !== ints || uiAgain.asIntArray() !== ints) return "int backing identity"
    val boxedInt: Any = keepUnsignedView(ui)
    if (boxedInt !is UIntArray || boxedInt is IntArray) return "int nominal identity"
    val intCheck = checkUnsignedViews(ui, uiAgain, uintArrayOf(UInt.MAX_VALUE, 2u), ints, 932)
    if (intCheck != "OK") return "int: $intCheck"
    ui[1] = 3u
    ints[0] = 1
    if (uiAgain[1] != 3u || ui[0] != 1u) return "int alias writes"
    if ((ui as Any).hashCode() != 995) return "int hash after mutation"

    val longs = longArrayOf(-1L, 2L)
    val ul = longs.asULongArray()
    val ulAgain = longs.asULongArray()
    if (ul.asLongArray() !== longs || ulAgain.asLongArray() !== longs) return "long backing identity"
    val boxedLong: Any = keepUnsignedView(ul)
    if (boxedLong !is ULongArray || boxedLong is LongArray) return "long nominal identity"
    val longCheck = checkUnsignedViews(ul, ulAgain, ulongArrayOf(ULong.MAX_VALUE, 2uL), longs, 963)
    if (longCheck != "OK") return "long: $longCheck"
    ul[1] = 3uL
    longs[0] = 1L
    if (ulAgain[1] != 3uL || ul[0] != 1uL) return "long alias writes"
    if ((ul as Any).hashCode() != 995) return "long hash after mutation"
    val emptyFirst: Any = UIntArray(0)
    val emptySecond: Any = UIntArray(0)
    if (emptyFirst == emptySecond) return "independent empty arrays equal"
    if (emptyFirst.hashCode() != 1 || emptySecond.hashCode() != 1) return "empty content hash"
    val absent: Any? = null
    if (boxedInt == absent) return "unsigned view equals null"
    return "OK"
}
