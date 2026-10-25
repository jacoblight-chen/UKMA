import { readFile } from "fs";

export class Graph {
    add(name: string): boolean {
        return this.check(name);
    }

    check(name: string): boolean {
        return name.length > 0;
    }
}

export function compute(size: number): number {
    return size + 1;
}
