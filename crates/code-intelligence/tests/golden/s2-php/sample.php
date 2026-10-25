<?php
namespace Demo;

use Foo\Bar;

class Graph
{
    public function add(string $name): bool
    {
        return $this->check($name);
    }

    public function check(string $name): bool
    {
        return strlen($name) > 0;
    }
}

function compute(int $size): int
{
    return $size + 1;
}
