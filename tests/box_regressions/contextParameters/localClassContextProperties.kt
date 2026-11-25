// LANGUAGE: +ContextParameters
// WITH_STDLIB
// Properties with context parameters declared by a local class: explicit and implicit reads and
// writes, through the declaring class and a local subclass, each selecting its site's context values.

fun box(): String = context(1, "K") {
    open class A(val tag: String) {
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

        fun implicit(): String = context(2, "L") {
            mutable = "i"
            prop + mutable + this@A.prop
        }
    }

    class B : A("b")

    val a = A("a")
    if (a.prop != "aK1") return@context "FAIL 1: ${a.prop}"
    a.mutable = "m"
    if (a.stored != "mK") return@context "FAIL 2: ${a.stored}"
    if (a.mutable != "mKK") return@context "FAIL 3: ${a.mutable}"

    val b = B()
    val implicit = b.implicit()
    if (implicit != "bL2iLLbL2") return@context "FAIL 4: $implicit"
    if (b.prop != "bK1") return@context "FAIL 5: ${b.prop}"
    b.mutable = "x"
    if (b.mutable != "xKK") return@context "FAIL 6: ${b.mutable}"
    "OK"
}
