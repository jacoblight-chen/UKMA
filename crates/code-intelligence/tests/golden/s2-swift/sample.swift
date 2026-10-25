import Foundation

class Graph {
    func add(_ name: String) -> Bool {
        return check(name)
    }

    func check(_ name: String) -> Bool {
        return !name.isEmpty
    }
}

func compute(size: Int) -> Int {
    return size + 1
}
