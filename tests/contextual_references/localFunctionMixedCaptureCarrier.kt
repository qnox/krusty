// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// PLATFORM-SPECIFIC: reads the JVM reference carrier's fields, constructor and `invoke` methods
// through Java reflection; the portable mixed-capture program is
// `box_regressions/contextParameters/localFunctionMixedCaptureReferences.kt`.

context(s: String)
fun go(): (String) -> String {
    var captured = "O"
    context(t: String)
    fun local(suffix: String) = captured + t + s + suffix
    val ref: (String) -> String = ::local
    captured = "Q"
    return ref
}

fun box(): String {
    val ref = context("_") { go() }
    if (ref("K") != "Q__K") return "FAIL value: ${ref("K")}"
    val carrier = ref.javaClass
    val fields = carrier.declaredFields.map { it.name + ":" + it.type.name }.sorted()
    if (fields != listOf("\$captured:kotlin.jvm.internal.Ref\$ObjectRef", "\$s:java.lang.String")) {
        return "FAIL fields: $fields"
    }
    val constructor = carrier.declaredConstructors.single().parameterTypes.map { it.name }
    if (constructor != listOf("kotlin.jvm.internal.Ref\$ObjectRef", "java.lang.String", "[Ljava.lang.Object;")) {
        return "FAIL constructor: $constructor"
    }
    val invokes = carrier.declaredMethods
        .filter { it.name == "invoke" }
        .map { it.returnType.name + "(" + it.parameterTypes.joinToString { type -> type.name } + ")" }
        .sorted()
    if (invokes != listOf("java.lang.Object(java.lang.Object)", "java.lang.String(java.lang.String)")) {
        return "FAIL invokes: $invokes"
    }
    return "OK"
}
