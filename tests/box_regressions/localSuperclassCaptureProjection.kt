fun captureDelegation(value: String): String {
    open class Parent {
        fun inherited() = value
    }
    class Child : Parent() {
        fun own() = value
    }
    val child = Child()
    return if (child.inherited() == "OK" && child.own() == "OK") "OK" else "fail"
}

fun box(): String = captureDelegation("OK")
