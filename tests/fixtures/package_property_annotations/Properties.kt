package store

@Target(AnnotationTarget.PROPERTY)
@Retention(AnnotationRetention.BINARY)
annotation class Mark(val value: Int)
@Target(AnnotationTarget.FIELD)
annotation class FieldMark
@Target(AnnotationTarget.PROPERTY_GETTER)
annotation class GetterMark

@Mark(1)
val literal: Int = 1

@Mark(2)
val computed: Int get() = 2

@Mark(3)
var mutable: Int = 3

@Mark(4)
private val hidden: Int = 4

@JvmInline
value class Meter(val n: Int)

@Mark(12)
val Meter.marked: Int get() = n

@Mark(5)
val String.extension: Int get() = length

@Mark(6)
val <T : Number> T.bounded: Int get() = toInt()

@field:FieldMark
@get:GetterMark
val targeted: Int = 7

@Mark(8)
const val constant: Int = 8

context(prefix: String)
@Mark(9)
val contextual: Int get() = prefix.length

@get:JvmName("readNamed")
@Mark(11)
val named: Int = 11

@Suppress("VOLATILE_ON_VALUE")
@Volatile val suppressed: Int = 10
