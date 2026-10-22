import java.util.*;

/** Interpreter micro-benchmarks shaped like Minecraft hot code. */
public class Bench {
    interface Density { float sample(int x, int y, int z); }
    static final class Constant implements Density { final float v; Constant(float v) { this.v = v; } public float sample(int x, int y, int z) { return v; } }
    static final class Add implements Density {
        final Density a, b; Add(Density a, Density b) { this.a = a; this.b = b; }
        public float sample(int x, int y, int z) { return a.sample(x, y, z) + b.sample(x, y, z); }
    }
    static final class Noise implements Density {
        final int[] perm = new int[512];
        Noise(long seed) { Random r = new Random(seed); for (int i = 0; i < 256; i++) perm[i] = perm[i + 256] = r.nextInt(256); }
        public float sample(int x, int y, int z) {
            int h = perm[(perm[(perm[x & 255] + y) & 255] + z) & 255];
            return (h & 15) / 7.5f - 1f;
        }
    }
    enum Axis { X, Y, Z; int choose(int x, int y, int z) { return switch (this) { case X -> x; case Y -> y; case Z -> z; }; } }
    static final class Vec { private final double x, y, z; Vec(double x, double y, double z) { this.x = x; this.y = y; this.z = z; }
        double x() { return x; } double y() { return y; } double z() { return z; }
        Vec add(Vec o) { return new Vec(x + o.x, y + o.y, z + o.z); } }

    static long density() {
        Density f = new Add(new Add(new Noise(1), new Constant(0.25f)), new Add(new Noise(2), new Noise(3)));
        float sum = 0;
        for (int x = 0; x < 64; x++) for (int y = 0; y < 64; y++) for (int z = 0; z < 64; z++) sum += f.sample(x, y, z);
        return (long) sum;
    }
    static long getters() {
        Vec v = new Vec(1, 2, 3); double sum = 0;
        for (int i = 0; i < 1_000_000; i++) sum += v.x() + v.y() + v.z();
        return (long) sum;
    }
    static long enums() {
        Axis[] axes = Axis.values(); long sum = 0;
        for (int i = 0; i < 1_000_000; i++) { Axis a = axes[i % 3]; sum += a.ordinal() + a.choose(i, i + 1, i + 2); }
        return sum;
    }
    static long arrays() {
        int[] a = new int[4096]; long sum = 0;
        for (int round = 0; round < 200; round++) for (int i = 1; i < a.length; i++) { a[i] = a[i - 1] * 31 + i; sum += a[i] & 7; }
        return sum;
    }
    static long floats() {
        double s = 0;
        for (int i = 1; i < 1_000_000; i++) { double t = i * 0.001; s += t * t / (1 + t) - Math.floor(t); }
        return (long) s;
    }
    static long allocation() {
        Vec v = new Vec(0, 0, 0); Vec d = new Vec(1, 0.5, 0.25);
        for (int i = 0; i < 300_000; i++) v = v.add(d);
        return (long) (v.x() + v.y() + v.z());
    }
    static long collections() {
        HashMap<Integer, Integer> map = new HashMap<>(); ArrayList<Integer> list = new ArrayList<>(); long sum = 0;
        for (int i = 0; i < 100_000; i++) { map.merge(i & 1023, 1, Integer::sum); list.add(i); }
        for (int v : map.values()) sum += v;
        for (int v : list) sum += v & 3;
        return sum;
    }
    static long strings() {
        long sum = 0;
        for (int i = 0; i < 20_000; i++) { String s = "block_" + i + "_state"; sum += s.hashCode() & 0xFF; sum += s.length(); }
        return sum;
    }

    interface Kernel { long run(); }

    public static void main(String[] args) {
        String[] names = {"density", "getters", "enums", "arrays", "floats", "allocation", "collections", "strings"};
        Kernel[] kernels = {Bench::density, Bench::getters, Bench::enums, Bench::arrays, Bench::floats, Bench::allocation, Bench::collections, Bench::strings};
        long total = 0;
        for (int k = 0; k < kernels.length; k++) {
            kernels[k].run(); // warm-up (class loading, linking)
            long start = System.nanoTime();
            long result = kernels[k].run();
            long ms = (System.nanoTime() - start) / 1_000_000;
            total += ms;
            System.out.printf("%-12s %6d ms   (result %d)%n", names[k], ms, result);
        }
        System.out.printf("%-12s %6d ms%n", "total", total);
    }
}
