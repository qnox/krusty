fun box(): String {
    val growable = mutableListOf(1, 2, 3, 4, 5)
    val range = growable.subList(1, 4)
    range[0] = 20
    if (growable != listOf(1, 20, 3, 4, 5)) return "fail set $growable"
    range.add(9)
    if (growable != listOf(1, 20, 3, 4, 9, 5) || range.size != 4) return "fail add $growable $range"
    range.add(1, 7)
    if (growable != listOf(1, 20, 7, 3, 4, 9, 5)) return "fail add at $growable"
    if (range.removeAt(0) != 20 || range.remove(9) != true) return "fail remove"
    if (growable != listOf(1, 7, 3, 4, 5) || range != listOf(7, 3, 4)) return "fail removed $growable $range"
    range.clear()
    if (growable != listOf(1, 5) || range.size != 0) return "fail clear $growable"
    val inner = growable.subList(0, 2).subList(1, 2)
    inner.add(8)
    if (growable != listOf(1, 5, 8)) return "fail nested $growable"
    return "OK"
}
