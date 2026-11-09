// Constructing a class initializes its companion object first, once: directly, through a
// subclass, through an object extending it, and through each constructor. A companion whose
// holder is never constructed or named stays uninitialized.
var log = ""

open class Holder(val tag: String) {
    constructor() : this("secondary")

    companion object {
        init {
            log += "c"
        }
    }

    init {
        log += "h"
    }
}

class Child : Holder("child") {
    init {
        log += "k"
    }
}

object Single : Holder("single")

class Unused {
    companion object {
        init {
            log += "u"
        }
    }
}

fun box(): String {
    log += "s"
    Holder("first")
    Holder()
    Child()
    Single.tag
    return if (log == "schhhkh") "OK" else log
}
