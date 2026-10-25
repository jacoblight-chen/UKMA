import scala.collection.mutable

class Graph {
  def add(name: String): Boolean = {
    check(name)
  }

  def check(name: String): Boolean = {
    name.nonEmpty
  }
}

object Helper {
  def compute(size: Int): Int = {
    size + 1
  }
}
