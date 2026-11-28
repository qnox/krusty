fun box(): String {
    val list = ArrayList<Int>()
    list.add(1)
    list.add(2)
    list.add(3)
    val answer = try {
        list.addAll((list as List<Int>).subList(0, 2))
        "appended"
    } catch (e: ConcurrentModificationException) {
        "raised"
    }
    return "$answer $list"
}
