// kotlinc analyzes a method for redundant casts without knowing the exact class of `this`, so a
// cast of a value read from `this` stays, while the same cast of a parameter or of a constructed
// value goes.
open class C(val tag: String) {
    fun viaAny(): C {
        val any: Any = this
        return any as C
    }

    fun viaLocal(): C {
        val self = this
        val any: Any = self
        return any as C
    }

    fun parameter(other: C): C {
        val any: Any = other
        return any as C
    }

    fun constructed(): C {
        val any: Any = C("n")
        return any as C
    }
}

object Single {
    fun viaAny(): Single {
        val any: Any = this
        return any as Single
    }
}

fun box(): String {
    val c = C("c")
    if (c.viaAny() !== c || c.viaLocal() !== c || c.parameter(c) !== c) return "FAIL: identity"
    if (c.constructed().tag != "n") return "FAIL: constructed"
    if (Single.viaAny() !== Single) return "FAIL: object"
    return "OK"
}
