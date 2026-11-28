// A local class's member signatures and supertype arguments see the nested classifiers of the
// class whose member function declares it. A local data class prints its own declared name.
sealed interface Result<out T, out E> {
    interface Ok<out T> : Result<T, Nothing> { val value: T }
    interface Error<out E> : Result<Nothing, E> { val error: E }
}

class Api {
    interface Problem { val message: String }

    fun run(fail: Boolean): Result<String, Problem> {
        data class Failure(override val error: Problem) : Result.Error<Problem>
        data class Success(override val value: String) : Result.Ok<String>
        class Message(override val message: String) : Problem, Comparable<Problem> {
            override fun compareTo(other: Problem): Int = message.compareTo(other.message)
            fun same(other: Problem): Boolean = compareTo(other) == 0
        }
        val problem = Message("K")
        check(problem.same(Message("K")))
        return if (fail) Failure(problem) else Success("O")
    }
}

// A nested classifier named like the declaring function is not an enclosing scope of its local
// class: `Token` is `Outer.Token`, not `Outer.make.Token`.
class Outer {
    interface Token { fun text(): String = "outer" }

    class make {
        interface Token { fun text(): String }
    }

    fun make(seed: Int): String {
        class Local : Token
        return Local().text() + seed
    }
}

fun describe(): String {
    data class Failure(val error: String)
    return Failure("x").toString()
}

fun box(): String {
    if (describe() != "Failure(error=x)") return describe()
    if (Outer().make(1) != "outer1") return Outer().make(1)
    val api = Api()
    val ok = api.run(false) as Result.Ok<String>
    val error = api.run(true) as Result.Error<Api.Problem>
    if (error.toString() != "Failure(error=${error.error})") return error.toString()
    return ok.value + error.error.message
}
