open class Animal
class Dog : Animal()
class Cat : Animal()
interface Named
class Pet : Animal(), Named

fun accepts(token: kotlin.reflect.KClass<*>, value: Any?): Boolean = token.isInstance(value)

fun box(): String {
    val e: Any? = IllegalStateException("boom")
    if (!accepts(IllegalStateException::class, e)) return "fail: same class"
    if (!accepts(RuntimeException::class, e)) return "fail: supertype"
    if (accepts(NumberFormatException::class, e)) return "fail: unrelated"
    if (accepts(IllegalStateException::class, null)) return "fail: null"
    val pet: Any = Pet()
    if (!accepts(Pet::class, pet)) return "fail: own class"
    if (!accepts(Animal::class, pet)) return "fail: own supertype"
    if (!accepts(Named::class, pet)) return "fail: interface"
    if (accepts(Cat::class, pet)) return "fail: sibling"
    if (accepts(Named::class, Dog())) return "fail: interface not implemented"
    if (!accepts(Any::class, pet)) return "fail: any"
    if (!accepts(String::class, "text")) return "fail: string"
    if (accepts(String::class, 1)) return "fail: not a string"
    val dog: Animal = Dog()
    if (!accepts(dog::class, Dog())) return "fail: value token own class"
    if (!accepts(dog::class, Dog())) return "fail: value token"
    if (accepts(dog::class, Cat())) return "fail: value token sibling"
    if (accepts(dog::class, Animal())) return "fail: value token supertype instance"
    return "OK"
}
