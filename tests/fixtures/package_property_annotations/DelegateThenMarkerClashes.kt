package store

import kotlin.reflect.KProperty

@Target(AnnotationTarget.PROPERTY)
annotation class Mark

operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1

val x: Int by Unit
fun `getX$delegate`(): Any? = Unit

@Mark val String.value: Int get() = length
fun `getValue$annotations`(value: String) {}
