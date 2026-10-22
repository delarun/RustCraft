import vmtest.Out;

public class Basics {
    interface Shape { double area(); default String describe() { return "shape with area " + (int) area(); } }
    record Rect(int w, int h) implements Shape { public double area() { return w * h; } }
    static abstract class Animal {
        abstract String sound();
        String speak() { return getClass().getName() + " says " + sound(); }
    }
    static class Dog extends Animal { String sound() { return "woof"; } }
    static class Puppy extends Dog { String sound() { return "yip"; } }

    // An indirect superinterface with a default method is initialised with
    // the class (JVMS §5.5), before its default method reads its statics.
    interface Listener { StringBuilder LOG = log("Listener initialised"); default void onError(String s) { LOG.append(s); } }
    interface SubListener extends Listener { void other(); }
    static class ListenerImpl implements SubListener { public void other() {} }
    static StringBuilder log(String s) { Out.println(s); return new StringBuilder(); }

    static int fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

    static long factorial(int n) { long r = 1; for (int i = 2; i <= n; i++) r *= i; return r; }

    static int depth(int n) { return n == 0 ? 0 : 1 + depth(n - 1); }

    public static void main(String[] args) {
        Out.println("implementing class");
        new ListenerImpl().onError("logged");
        Out.println("log: " + Listener.LOG);
        Out.println("fib(20) = " + fib(20));
        Out.println("20! = " + factorial(20));
        Out.println("int overflow: " + (Integer.MAX_VALUE + 1));
        Out.println("division: " + (-7 / 2) + " " + (-7 % 2) + " " + (7.0 / 2));
        Out.println("shifts: " + (-16 >> 2) + " " + (-16 >>> 28) + " " + (1L << 40));
        Out.println("casts: " + (int) 3.99 + " " + (int) Double.NaN + " " + (long) 1e30 + " " + (byte) 200 + " " + (char) 65);
        Out.println("floats: " + 0.1f + " " + (0.1 + 0.2) + " " + 1e-10 + " " + 100.0 / 3);
        Out.println("compare: " + Double.compare(0.0, -0.0) + " " + (Double.NaN < 1) + " " + Long.compare(5, 3));

        StringBuilder sb = new StringBuilder();
        for (char c = 'a'; c <= 'e'; c++) sb.append(c).append(',');
        sb.setLength(sb.length() - 1);
        Out.println(sb.reverse().toString());
        Out.println("unicode: " + "Привет, мир".length() + " " + "Привет".toUpperCase() + " " + "ß".toUpperCase());
        String s = "The quick brown fox";
        Out.println(s.substring(4, 9) + "|" + s.indexOf("fox") + "|" + s.replace('o', '0') + "|" + String.join("-", s.split(" ")));
        Out.println("hash: " + "hello".hashCode() + " " + "".hashCode());
        Out.println("equals: " + "abc".equals(new String("abc")) + " " + ("abc" == new String("abc").intern()));

        int[] arr = {5, 3, 9, 1, 7};
        java.util.Arrays.sort(arr);
        Out.println(java.util.Arrays.toString(arr));
        int[][] grid = new int[3][4];
        grid[2][3] = 42;
        Out.println("grid: " + grid.length + "x" + grid[0].length + " " + grid[2][3]);
        Object[] objects = new String[2];
        try {
            objects[0] = 1.5;
        } catch (ArrayStoreException e) {
            Out.println("ArrayStoreException: " + e.getMessage());
        }

        Animal[] animals = {new Dog(), new Puppy()};
        for (Animal a : animals) Out.println(a.speak());
        Shape r = new Rect(3, 4);
        Out.println(r.describe() + " " + ((Rect) r).w());
        Out.println("instanceof: " + (r instanceof Shape) + " " + (animals[1] instanceof Dog) + " " + (animals instanceof Object[]));

        java.util.HashMap<String, String> map = new java.util.HashMap<>();
        for (String word : "one two three two one one".split(" ")) {
            String count = map.get(word);
            map.put(word, count == null ? "1" : count + "1");
        }
        java.util.ArrayList<String> keys = new java.util.ArrayList<>(map.keySet());
        java.util.Collections.sort(keys);
        for (String k : keys) Out.println(k + " -> " + map.get(k).length());

        try {
            Object o = "string";
            Integer bad = (Integer) o;
            Out.println("unreachable " + bad);
        } catch (ClassCastException e) {
            Out.println("ClassCastException caught");
        }
        try {
            int[] small = new int[2];
            small[5] = 1;
        } catch (ArrayIndexOutOfBoundsException e) {
            Out.println("AIOOBE: " + e.getMessage());
        }
        try {
            Out.println(10 / (args.length));
        } catch (ArithmeticException e) {
            Out.println("ArithmeticException: " + e.getMessage());
        }
        try {
            String n = null;
            n.length();
        } catch (NullPointerException e) {
            Out.println("NPE caught");
        }
        try {
            throw new IllegalStateException("custom", new RuntimeException("cause"));
        } catch (IllegalStateException e) {
            Out.println(e.getMessage() + " / " + e.getCause().getMessage());
        } finally {
            Out.println("finally ran");
        }
        Out.println("recursion depth: " + depth(1000));
        switch (args.length) {
            case 0 -> Out.println("no args");
            case 1 -> Out.println("one arg");
            default -> Out.println("many args");
        }
        Out.println(Basics.class.getName() + " " + int[].class.getName() + " " + String[][].class.getName() + " " + int.class.getName());
        Out.println(new Puppy().getClass().getSuperclass().getSimpleName());
        try {
            for (java.lang.reflect.RecordComponent c : Rect.class.getRecordComponents()) {
                Out.println("record component " + c.getName() + ": " + c.getType() + " = " + c.getAccessor().invoke(new Rect(5, 7)));
            }
        } catch (ReflectiveOperationException e) {
            Out.println("reflection failed: " + e);
        }
    }
}
