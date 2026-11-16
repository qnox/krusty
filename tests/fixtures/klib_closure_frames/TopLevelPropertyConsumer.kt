fun box(): String {
    if (bump() != 1) return "bump"
    counter = 10
    if (bump() != 11) return "bump after set"
    if (counter != 11) return "read after bump"
    if (doubled != 22) return "computed"
    if (greeting != "hi") return "val"
    if (remember("a") != null) return "first remember"
    if (remember("b") != "a") return "second remember"
    if (last != "b") return "last"
    last = null
    if (remember("c") != null) return "reset"
    if (limit() != 3L) return "const"
    if (clamp(9) != 5) return "clamp"
    clamped = 2
    if (clamped != 2) return "clamped set"
    return "OK"
}
