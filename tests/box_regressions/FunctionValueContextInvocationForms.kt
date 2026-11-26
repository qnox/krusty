// A function value with a context parameter and an extension receiver takes three invocation
// forms: every argument explicit, the context from scope, and both the context and the receiver
// from scope. Expression bodies infer their result through each form.

class Ctx(val tag: String)

class Rec(val name: String)

context(c: Ctx)
fun tag(): String = c.tag

fun allExplicit(f: context(Ctx) Rec.(Int) -> String, c: Ctx, r: Rec) = f(c, r, 1)

fun contextFromScope(f: context(Ctx) Rec.(Int) -> String, c: Ctx, r: Rec) = with(c) { f(r, 2) }

fun explicitReceiver(f: context(Ctx) Rec.(Int) -> String, c: Ctx, r: Rec) = with(c) { r.f(3) }

fun bothFromScope(f: context(Ctx) Rec.(Int) -> String, c: Ctx, r: Rec) =
    with(c) { with(r) { f(4) } }

fun box(): String {
    val f: context(Ctx) Rec.(Int) -> String = { tag() + name + it }
    val c = Ctx("c")
    val r = Rec("r")
    val result = allExplicit(f, c, r) + contextFromScope(f, c, r) +
        explicitReceiver(f, c, r) + bothFromScope(f, c, r)
    return if (result == "cr1cr2cr3cr4") "OK" else result
}
