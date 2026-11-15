// LANGUAGE: +CompanionBlocks
// A companion block property is initialized with its class, not at program start: on the class's
// first construction (before the constructor's arguments), block function call, or storage access,
// after its superclass and the superclass's companion object, and only once.

var log = ""

fun note(step: String): Int {
    log += step
    return log.length
}

open class Base {
    companion {
        val base = note("B")
    }
}

class Derived : Base() {
    companion {
        val derived = note("D")
        var counter = note("C")
    }
}

class Called {
    companion {
        val value = note("F")
        fun read(): Int = value
    }
}

class Accessed {
    companion {
        val value = note("A")
    }
}

class Argued(val argument: Int) {
    companion {
        val value = note("G")
    }
}

open class WithCompanion {
    companion object {
        init {
            note("W")
        }
    }
}

class Child : WithCompanion() {
    companion {
        val value = note("H")
    }
}

fun box(): String {
    note("0")
    if (log != "0") return "Fail: initialized at program start: $log"
    Derived()
    if (log != "0BDC") return "Fail: construction: $log"
    Derived()
    if (log != "0BDC") return "Fail: initialized twice: $log"
    if (Called.read() != 5) return "Fail: block function: $log"
    note("1")
    if (Accessed.value != 7) return "Fail: storage access: $log"
    Derived.counter = 10
    if (Derived.counter != 10 || Base.base != 2 || Derived.derived != 3) return "Fail: values"
    if (log != "0BDCF1A") return "Fail: $log"
    Argued(note("2"))
    if (log != "0BDCF1AG2") return "Fail: constructor arguments: $log"
    if (Child.value != 11) return "Fail: superclass companion: $log"
    return if (log == "0BDCF1AG2WH") "OK" else "Fail: $log"
}
