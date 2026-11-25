// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// TARGET_BACKEND: JVM
// WITH_STDLIB
// A reference binds a value-class context argument as a box, while the accessor it calls takes
// the class's carrier under a value-class-mangled name. Each accessor shape is covered: a
// top-level getter and setter, a member, and an extension with two contexts. A nullable context
// over a reference carrier passes a bound `null` through to that carrier unchanged.

@JvmInline
value class Z(val value: String)

@JvmInline
value class N(val value: Int)

context(z: Z)
val decorated: String
    get() = "[" + z.value + "]"

var log = ""

context(z: Z)
var tagged: String
    get() = z.value + log
    set(value) {
        log = z.value + value
    }

class Host(val prefix: String) {
    context(z: Z)
    val member: String
        get() = prefix + z.value
}

context(z: Z?)
val maybe: String
    get() = z?.value ?: "none"

context(z: Z, n: N)
val String.joined: String
    get() = this + z.value + n.value

fun box(): String {
    context(Z("O")) {
        val p = ::decorated
        if (p.get() != "[O]") return "FAIL 1: ${p.get()}"

        val t = ::tagged
        t.set("K")
        if (log != "OK") return "FAIL 2: $log"
        if (t.get() != "OOK") return "FAIL 3: ${t.get()}"

        val m = Host::member
        if (m.get(Host("h")) != "hO") return "FAIL 4: ${m.get(Host("h"))}"

        context(N(7)) {
            val e = String::joined
            if (e.get("s") != "sO7") return "FAIL 6: ${e.get("s")}"
        }
    }
    context<Z?, Unit>(null) {
        val absent = ::maybe
        if (absent.get() != "none") return "FAIL 7: ${absent.get()}"
    }
    context<Z?, Unit>(Z("P")) {
        val present = ::maybe
        if (present.get() != "P") return "FAIL 8: ${present.get()}"
    }
    return "OK"
}
