require "json"

class Graph
  def add(name)
    check(name)
  end

  def check(name)
    name.length > 0
  end
end

def compute(size)
  size + 1
end
