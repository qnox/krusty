package refused

// Members of one signature but different return types meet in a member that copies the more
// specific one, which is not described yet.
interface Left {
    fun wide(): Any
}

interface Right {
    fun wide(): String
}

abstract class Joined : Left, Right
