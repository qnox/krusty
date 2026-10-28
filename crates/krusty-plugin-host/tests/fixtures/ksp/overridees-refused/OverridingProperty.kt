package overridees

open class Base {
    open val label: String = ""
}

class Derived : Base() {
    override val label: String = "derived"
}
