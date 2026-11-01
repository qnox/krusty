// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Team(val members: List<String>)

class Repository(private val teams: Map<String, Team>) {
    suspend fun find(id: String): Team? = teams[id]
}

// The erased result of each suspension is checked before it is cast to the declared result.
suspend fun memberCount(repository: Repository, id: String): Int = repository.find(id)!!.members.size

// A tail result keeps the suspend function's erased `Object` return: it is checked, never cast.
suspend fun team(repository: Repository, id: String): Team = repository.find(id)!!

fun run(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    val repository = Repository(mapOf("a" to Team(listOf("O", "K"))))
    var result = "not run"
    run {
        val count = memberCount(repository, "a")
        val members = team(repository, "a").members
        result = if (count == 2) members.joinToString("") else "count $count"
    }
    return result
}
