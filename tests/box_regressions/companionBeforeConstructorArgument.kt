// `new` initializes the class before constructor arguments are evaluated. The companion
// runs, then the argument, then the instance initializer.

var log = ""

class Holder(n: Int) {
    init { log += "I" }
    companion object { init { log += "C" } }
}

fun argument(): Int { log += "A"; return 0 }

fun box(): String {
    Holder(argument())
    return if (log == "CAI") "OK" else log
}
