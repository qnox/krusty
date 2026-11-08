fun check(value: String): String {
    open class B { fun fromB() = value }
    class C { fun fromC() = value }
    class A : B() {
        fun own() = value
        fun requestedB(value: String) = if (value == "shadow") B().fromB() else value
        fun requestedC(value: String) = if (value == "shadow") C().fromC() else value
    }
    val a = A()
    return if (a.own() == "OK" && a.fromB() == "OK" &&
        a.requestedB("shadow") == "OK" && a.requestedC("shadow") == "OK") "OK" else "fail"
}
fun box() = check("OK")
