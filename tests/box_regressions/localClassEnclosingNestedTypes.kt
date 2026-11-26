// A local class's member signatures and supertype arguments see the nested classifiers of the
// class whose member function declares it.
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

fun box(): String {
    val api = Api()
    val ok = api.run(false) as Result.Ok<String>
    val error = api.run(true) as Result.Error<Api.Problem>
    return ok.value + error.error.message
}
