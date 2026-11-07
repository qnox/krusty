package store

@Target(AnnotationTarget.PROPERTY, AnnotationTarget.FIELD)
@Retention(AnnotationRetention.BINARY)
annotation class Stamp(val value: String)

@property:Stamp("property")
@field:Stamp("field")
const val answer: Int = 42

@property:Stamp("string-property")
@field:Stamp("string-field")
const val label: String = "constant"
