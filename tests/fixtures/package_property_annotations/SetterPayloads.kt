package payloads

annotation class Leaf(val text: String)

@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class Payload(val text: String, val nested: Leaf, val values: Array<String>)

@Retention(AnnotationRetention.BINARY)
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class BinaryPayload(val count: Int, val nested: Leaf)

@setparam:Payload("runtime", Leaf("nested"), ["a", "b"])
@setparam:BinaryPayload(7, Leaf("binary"))
var nullable: String? = null
