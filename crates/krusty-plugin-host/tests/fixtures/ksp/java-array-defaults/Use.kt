package ja

import javaarrays.ArrayElements
import javaarrays.Valued

@ArrayElements(names = ["a"], count = 1, types = [String::class], numbers = [2])
class AllWritten

@ArrayElements(count = 3, types = [], names = [], numbers = [], tags = ["y"])
class Empty

@Valued("v")
class Value

@Valued
class Omitted
