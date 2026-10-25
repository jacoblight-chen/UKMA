#include <vector>

namespace demo {

class Graph {
public:
    int add(int a) {
        return check(a);
    }

    int check(int a) {
        return a + 1;
    }
};

int compute(int size) {
    return size + 1;
}

}
