package store

import kotlin.reflect.KProperty

@Target(AnnotationTarget.PROPERTY)
annotation class Mark

operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1

@Mark val String.value: Int get() = length
fun `getValue$annotations`(value: String) {}

val x: Int by Unit
fun `getX$delegate`(): Any? = Unit
