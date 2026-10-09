package ty
typealias Name = String
typealias Pairs<A> = Map<A, List<A>>
fun greet(n: Name): Pairs<Name> = mapOf(n to listOf(n))
internal typealias Hidden = Int
