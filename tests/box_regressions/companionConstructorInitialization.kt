// Constructing a class is its first use of the companion. The companion's initializer runs
// before the instance initializer, from a primary constructor and from a secondary one, and a
// later construction does not run it again.

var log = ""

class Primary(val n: Int) {
    init {
        log += "p$n"
    }

    companion object {
        init {
            log += "c"
        }
    }
}

class Secondary {
    constructor(n: Int) {
        log += "s$n"
    }

    companion object {
        init {
            log += "k"
        }
    }
}

fun box(): String {
    Primary(1)
    if (log != "cp1") return "primary:$log"
    Secondary(2)
    if (log != "cp1ks2") return "secondary:$log"
    Primary(3)
    if (log != "cp1ks2p3") return "repeat:$log"
    return "OK"
}
