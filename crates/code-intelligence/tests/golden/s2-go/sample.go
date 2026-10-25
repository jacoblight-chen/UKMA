package main

import "fmt"

type Graph struct {
	Name string
}

func (g Graph) Add(name string) bool {
	return g.Check(name)
}

func (g Graph) Check(name string) bool {
	return len(name) > 0
}

func compute(size int) int {
	return size + 1
}
