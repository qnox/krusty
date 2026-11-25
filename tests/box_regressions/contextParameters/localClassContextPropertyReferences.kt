// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// WITH_STDLIB
// References to properties with context parameters declared by a local class bind the context
// values at the reference site; the receiver stays an input of the reference.

fun box(): String = context(1, "K") {
    class A(val tag: String) {
        context(c1: Int, c2: String)
        val prop: String
            get() = tag + c2 + c1

        context(c2: String)
        var mutable: String
            get() = stored + c2
            set(value) {
                stored = value + c2
            }

        var stored = ""

        val delegated by A::prop
    }

    val a = A("a")
    if (a.delegated != "aK1") return@context "FAIL 1: ${a.delegated}"
    val read = A::prop
    if (read.get(A("r")) != "rK1") return@context "FAIL 2: ${read.get(A("r"))}"
    val write = A::mutable
    write.set(a, "w")
    if (a.stored != "wK") return@context "FAIL 3: ${a.stored}"
    if (write.get(a) != "wKK") return@context "FAIL 4: ${write.get(a)}"
    val bound = a::prop
    if (bound.get() != "aK1") return@context "FAIL 5: ${bound.get()}"
    "OK"
}
