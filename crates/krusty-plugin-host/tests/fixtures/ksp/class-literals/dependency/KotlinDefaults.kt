package kotlinclasses

import kotlin.reflect.KClass

annotation class KotlinDefaults(
    val none: KClass<*> = Unit::class,
    val primitive: KClass<*> = Int::class,
    val list: KClass<*> = List::class,
    val mutable: KClass<*> = MutableMap::class,
    val array: KClass<*> = IntArray::class,
    val objects: KClass<*> = Array<String>::class,
    val bounded: KClass<out Number> = Long::class,
)
