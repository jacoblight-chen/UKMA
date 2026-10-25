import os


class Graph:
    def add(self, name):
        return self.check(name)

    def check(self, name):
        return len(name) > 0


def compute(size):
    return size + 1
