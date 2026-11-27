import kotlin.reflect.KClass

class Plain

fun isKClass(value: Any?): Boolean = value is KClass<*>

fun asKClass(value: Any?): KClass<*>? = value as? KClass<*>

fun box(): String {
    if (!isKClass(Plain::class)) return "fail class literal"
    if (!isKClass(String::class)) return "fail String literal"
    if (isKClass(Plain())) return "fail instance"
    if (isKClass("text")) return "fail String value"
    if (isKClass(null)) return "fail null"
    if (asKClass(Plain::class) != Plain::class) return "fail safe cast"
    if (asKClass(Plain()) != null) return "fail safe cast instance"
    if (asKClass(null) != null) return "fail safe cast null"
    return "OK"
}
