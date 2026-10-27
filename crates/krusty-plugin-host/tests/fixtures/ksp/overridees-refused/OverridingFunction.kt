package overridees

open class Base {
    open fun hook() {}
}

class Derived : Base() {
    override fun hook() {}
}
