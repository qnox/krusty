// `this is B` on a receiver of type `M : A` with `B : A` narrows toward `B`: a bare member
// that `B` overrides covariantly resolves to the narrowed declaration, not the bound's.
enum class Permission { MEMBER, OWNER }

interface User { val name: String }
interface Member : User { val permission: Permission }

sealed interface Event { val sender: User }
class GroupEvent(override val sender: Member) : Event

fun <M : Event> M.isOwner(): Boolean = this is GroupEvent && sender.permission == Permission.OWNER

fun <M : Event> inLambda(event: M): Boolean = with(event) { this is GroupEvent && sender.permission == Permission.OWNER }

class Filter<M : Event> {
    fun test(event: M, check: M.() -> Boolean): Boolean = event.check()
    fun owner(event: M): Boolean = test(event) { this is GroupEvent && sender.permission == Permission.OWNER }
}

fun box(): String {
    val owner = GroupEvent(object : Member {
        override val name = "o"
        override val permission = Permission.OWNER
    })
    if (!owner.isOwner()) return "extension"
    if (!inLambda(owner)) return "with"
    if (!Filter<Event>().owner(owner)) return "class parameter"
    return "OK"
}
