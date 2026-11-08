fun captureLocal(value: String): String {
    class A {
        fun own() = value
        fun local(value: String): String {
            class B { fun read() = value }
            val construct = { B() }
            return construct().read()
        }
    }
    val a = A()
    return if (a.own() == "outer" && a.local("OK") == "OK") "OK" else "fail"
}
fun box() = captureLocal("outer")
