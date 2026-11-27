// LANGUAGE: +CompanionBlocksAndExtensions
// A compound assignment to a companion-associated property reads and writes it through the
// classifier: a companion extension `var` and a companion block `var`.

private var stored = 1

class C

companion var C.extension: Int
    get() = stored
    set(value) {
        stored = value
    }

class D {
    companion {
        var block: Int = 10
    }
}

fun box(): String {
    C.extension += 2
    if (C.extension != 3) return "Fail extension: ${C.extension}"
    C.extension -= 1
    if (C.extension != 2) return "Fail minus: ${C.extension}"
    D.block += 5
    if (D.block != 15) return "Fail block: ${D.block}"
    D.block *= 2
    if (D.block != 30) return "Fail times: ${D.block}"
    return "OK"
}
