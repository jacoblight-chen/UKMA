import kotlin.math.abs

class Graph {
    fun add(name: String): Boolean {
        return check(name)
    }

    fun check(name: String): Boolean {
        return name.isNotEmpty()
    }
}

fun compute(size: Int): Int {
    return size + 1
}
