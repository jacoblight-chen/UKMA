using System;

namespace Demo
{
    public class Graph
    {
        public bool Add(string name)
        {
            return this.Check(name);
        }

        private bool Check(string name)
        {
            return name.Length > 0;
        }
    }

    public static class Helper
    {
        public static int Compute(int size)
        {
            return size + 1;
        }
    }
}
