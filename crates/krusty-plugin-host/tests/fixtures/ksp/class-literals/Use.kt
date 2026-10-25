package jc

import javaclasses.ClassDefaults
import kotlin.reflect.KClass
import kotlinclasses.KotlinDefaults

annotation class SourceDefaults(
    val none: KClass<*> = Unit::class,
    val list: KClass<*> = List::class,
    val array: KClass<*> = IntArray::class,
)

@ClassDefaults
class Plain

@ClassDefaults(none = Unit::class, primitive = Int::class, list = MutableList::class)
class Written

@KotlinDefaults
class FromKotlin

@SourceDefaults
class FromSource
