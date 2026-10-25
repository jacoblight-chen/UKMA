#include <stdio.h>

struct Graph {
    int size;
};

int compute(int size) {
    return size + 1;
}

int main(void) {
    return compute(1);
}
