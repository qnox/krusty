// MODULE: lib
// FILE: model.kt
// A concrete extension property is more specific than a generic one with an equally specific
// receiver classifier, so it wins at every access: explicit, implicit `this`, lambda receiver and
// write, from the declaring module and from a dependency.
interface ApplicationCall {
    val application: String
}

interface PipelineCall : ApplicationCall

open class Pipeline<TSubject : Any, TContext : Any>(val context: TContext, var subject: TSubject)

class Selected(override val application: String) : PipelineCall

// FILE: genericAccess.kt
val <C : ApplicationCall> Pipeline<*, C>.call: C
    get() = context

var <S : Any> Pipeline<S, *>.current: S
    get() = subject
    set(value) {
        subject = value
    }

// FILE: concreteAccess.kt
inline val Pipeline<*, PipelineCall>.call: PipelineCall
    get() = Selected("concrete:" + context.application)

var Pipeline<String, *>.current: String
    get() = "concrete:" + subject
    set(value) {
        subject = "concrete:" + value
    }

val Pipeline<*, PipelineCall>.libraryApplication: String
    get() = call.application

// MODULE: main(lib)
// FILE: genericLocal.kt
val <C : ApplicationCall> Pipeline<*, C>.local: String
    get() = "generic"

// FILE: main.kt
class Call(override val application: String) : PipelineCall

val Pipeline<*, PipelineCall>.local: String
    get() = "concrete"

val Pipeline<*, PipelineCall>.application: String
    get() = call.application

fun Pipeline<*, PipelineCall>.viaThis(): String = this.call.application + local

fun <T> withPipeline(
    pipeline: Pipeline<String, PipelineCall>,
    block: Pipeline<String, PipelineCall>.() -> T,
): T = pipeline.block()

fun box(): String {
    val pipeline = Pipeline<String, PipelineCall>(Call("app"), "s")
    val explicit = pipeline.call.application
    if (explicit != "concrete:app") return "fail explicit: $explicit"
    if (pipeline.application != "concrete:app") return "fail implicit: ${pipeline.application}"
    val library = pipeline.libraryApplication
    if (library != "concrete:app") return "fail library: $library"
    if (pipeline.viaThis() != "concrete:appconcrete") return "fail this: ${pipeline.viaThis()}"
    if (pipeline.local != "concrete") return "fail local: ${pipeline.local}"
    val viaLambda = withPipeline(pipeline) { call.application + local + current }
    if (viaLambda != "concrete:appconcreteconcrete:s") return "fail lambda: $viaLambda"
    pipeline.current = "w"
    if (pipeline.subject != "concrete:w") return "fail write: ${pipeline.subject}"
    return "OK"
}
