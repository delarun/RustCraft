import vmtest.Out;

// Virtual and interface calls whose receivers change classes after the
// caller was compiled (guarded inlining, polymorphic sites, recompilation).
public class Polymorphic {
    interface Shape {
        int area();
        default int twice() { return area() * 2; }
    }
    static class Square implements Shape {
        final int s;
        Square(int s) { this.s = s; }
        public int area() { return s * s; }
    }
    static class BigSquare extends Square { // inherits area()
        BigSquare(int s) { super(s + 10); }
    }
    static class Rect implements Shape {
        final int w, h;
        Rect(int w, int h) { this.w = w; this.h = h; }
        public int area() { return w * h; }
        public int twice() { return area() + area() + 1; }
    }
    static class Tri implements Shape {
        final int b, h;
        Tri(int b, int h) { this.b = b; this.h = h; }
        public int area() { return b * h / 2; }
    }
    static class Circle implements Shape {
        final int r;
        Circle(int r) { this.r = r; }
        public int area() { return 3 * r * r; }
    }
    static abstract class Node {
        abstract int value();
        int plusOne() { return value() + 1; }
    }
    static class Leaf extends Node { int value() { return 7; } }
    static class Pair extends Node {
        final Node a, b;
        Pair(Node a, Node b) { this.a = a; this.b = b; }
        int value() { return a.value() + b.value(); }
    }
    static class Neg extends Node {
        final Node n;
        Neg(Node n) { this.n = n; }
        int value() { return -n.value(); }
        int plusOne() { return 100; }
    }

    static long area(Shape s) { return s.area(); }
    static long twice(Shape s) { return s.twice(); }
    static long plusOne(Node n) { return n.plusOne(); }

    static long phase(Shape[] shapes, Node[] nodes, int rounds) {
        long sum = 0;
        for (int r = 0; r < rounds; r++) {
            for (Shape s : shapes) sum = sum * 31 + area(s) + twice(s);
            for (Node n : nodes) sum = sum * 31 + plusOne(n);
        }
        return sum;
    }

    public static void main(String[] args) {
        Node leaf = new Leaf();
        // One class each while the callers get compiled...
        Out.println("mono " + phase(new Shape[] {new Square(3)}, new Node[] {leaf}, 2000));
        // ...then a subclass that dispatches to the same method...
        Out.println("same " + phase(new Shape[] {new Square(3), new BigSquare(2)}, new Node[] {leaf}, 3000));
        // ...then two, three and more targets per site.
        Out.println("bi " + phase(new Shape[] {new Square(1), new Rect(2, 3)}, new Node[] {leaf, new Pair(leaf, leaf)}, 3000));
        Shape[] many = {new Square(4), new Rect(1, 5), new Tri(3, 4), new BigSquare(1), new Circle(2)};
        Node[] nodes = {leaf, new Pair(leaf, new Neg(leaf)), new Neg(new Pair(leaf, leaf))};
        Out.println("many " + phase(many, nodes, 5000));
        // Back to the first class.
        Out.println("mono again " + phase(new Shape[] {new Square(5)}, new Node[] {leaf}, 2000));
    }
}
