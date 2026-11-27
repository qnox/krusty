open class Animal
class Dog : Animal()
class Cat : Animal()

fun box(): String {
    val e: Any? = IllegalStateException("boom")
    if (!IllegalStateException::class.isInstance(e)) return "fail: same class"
    if (!RuntimeException::class.isInstance(e)) return "fail: supertype"
    if (NumberFormatException::class.isInstance(e)) return "fail: unrelated"
    if (IllegalStateException::class.isInstance(null)) return "fail: null"
    val pet: Any = Dog()
    if (!Dog::class.isInstance(pet)) return "fail: own class"
    if (!Animal::class.isInstance(pet)) return "fail: own supertype"
    if (Cat::class.isInstance(pet)) return "fail: sibling"
    if (!Any::class.isInstance(pet)) return "fail: any"
    if (!String::class.isInstance("text")) return "fail: string"
    if (String::class.isInstance(1)) return "fail: not a string"
    return "OK"
}
