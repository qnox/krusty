// The companion's initializer constructs the class. That constructor asks for the companion
// again while it is still being initialized, and the class initializer finishes afterwards.

var log = ""

class Host(val n: Int) {
    init {
        log += "h$n"
    }

    companion object {
        val first = Host(0)

        init {
            log += "c"
        }
    }
}

fun box(): String {
    if (Host.first.n != 0) return "value:${Host.first.n}"
    if (log != "h0c") return "order:$log"
    return "OK"
}
