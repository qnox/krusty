tailrec fun inTryBlock(n: Int): Int { try { return inTryBlock(n - 1) } catch (e: Exception) { return 0 } }
tailrec fun inCatchAndFinally(n: Int): Int = try { 1 } catch (e: Exception) { inCatchAndFinally(n - 1) } finally { inCatchAndFinally(n - 2) }
tailrec fun lambdaInTry(n: Int): Int { try { run { return lambdaInTry(n - 1) } } finally { } }
