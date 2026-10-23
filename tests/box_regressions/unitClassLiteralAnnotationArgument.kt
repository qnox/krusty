import kotlin.reflect.KClass

annotation class Kind(val value: KClass<*> = Unit::class)

@Kind(Unit::class)
class Written

@Kind
class Defaulted

fun box(): String {
    Written()
    Defaulted()
    return "OK"
}
