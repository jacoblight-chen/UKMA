package demo;

import java.util.List;


public class Graph {
    private int size;

    public boolean add(String name) {
        return this.check(name);
    }

    boolean check(String name) {
        return name.isEmpty();
    }
}

class Helper {
    static int compute(int size) {
        return size + 1;
    }
}
