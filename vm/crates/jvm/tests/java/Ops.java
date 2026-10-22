import java.util.*;
import vmtest.Out;

// frem/drem, aastore checks and class literals, run hot; compared with HotSpot.
public class Ops {
    interface Shape {}
    static class Circle implements Shape {}
    static class Square implements Shape {}
    public static void main(String[] args) {
        float[] fs = {5.5f, -5.5f, 0.0f, -0.0f, 1e30f, -1e-30f, Float.NaN, Float.POSITIVE_INFINITY, 3f, -7.25f, Float.MIN_VALUE, 1e7f};
        double[] ds = {5.5, -5.5, 0.0, -0.0, 1e300, -1e-300, Double.NaN, Double.NEGATIVE_INFINITY, 3, -7.25, Double.MIN_VALUE, 1e17};
        long fh = 0, dh = 0;
        for (int round = 0; round < 3000; round++) {
            for (float a : fs) for (float b : fs) fh = fh * 31 + Float.floatToRawIntBits(a % b);
            for (double a : ds) for (double b : ds) dh = dh * 31 + Double.doubleToRawLongBits(a % b);
        }
        Out.println("frem " + fh + " drem " + dh);
        Object[] objs = new Object[4];
        Shape[] shapes = new Shape[4];
        Circle[] circles = new Circle[4];
        Object[] aliasShapes = shapes;
        Object[] aliasCircles = circles;
        Object[] values = {new Circle(), new Square(), "s", null, 1};
        int stored = 0, failed = 0;
        for (int round = 0; round < 20000; round++) {
            Object v = values[round % values.length];
            for (Object[] target : new Object[][] {objs, aliasShapes, aliasCircles}) {
                try {
                    target[round & 3] = v;
                    stored++;
                } catch (ArrayStoreException e) {
                    failed++;
                }
            }
        }
        Out.println("stored " + stored + " failed " + failed + " " + Arrays.toString(objs).replaceAll("@\\w+", ""));
        int classes = 0;
        for (int i = 0; i < 100000; i++) {
            Class<?> c = (i & 1) == 0 ? Circle.class : int[].class;
            classes += c.getName().length() + (c == Circle.class ? 1 : 0) + (String.class.isInstance(values[i % 5]) ? 3 : 0);
        }
        Out.println("classes " + classes);
    }
}
