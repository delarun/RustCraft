import java.util.*;

/** Bytecode shapes that stress the JIT (run hot enough to be compiled). */
public class JitPatterns {
    int size = 10, modCount;
    long total;
    double scale = 1.5;
    long[] longs = new long[8];
    static int counter;

    // dup_x1 / dup on ints held in wasm locals (PriorityQueue.poll shape).
    int shrink(Object[] queue) {
        int s = --size;
        Object last = queue[s];
        queue[s] = null;
        modCount++;
        return s + (last == null ? 0 : 1);
    }

    // dup2_x1 / dup2 on longs and doubles: x.f += ..., a[i]++ on long[].
    long wide(int i) {
        total += i;
        scale *= 1.0000001;
        longs[i & 7]++;
        long t = longs[i & 7] += total;
        return t ^ (long) scale;
    }

    static int arith(int a, int b) {
        int r = 0;
        r += a / (b | 1);
        r += a % (b | 1);
        r += Integer.MIN_VALUE / -1;
        r += Integer.MIN_VALUE % -1;
        r += (int) (Long.MIN_VALUE / -1L);
        r += (int) (float) Float.NaN + (int) Double.POSITIVE_INFINITY;
        r += (int) -2.5e10 + (int) (long) 1e19;
        r += Float.compare(Float.NaN, 1f) + (Double.NaN > 1 ? 1 : 0) + (Double.NaN < 1 ? 1 : 0);
        r += (byte) a + (char) a + (short) a;
        r += a >> 33 + a >>> b + (a << -1);
        return r;
    }

    static int sw(int k) {
        switch (k) {
            case -3: return 1;
            case 0: return 2;
            case 1: return 3;
            case 2: return 5;
            case 4: return 7;
            default: return k == Integer.MIN_VALUE ? 11 : 13;
        }
    }

    static int lookup(int k) {
        switch (k) {
            case 100: return 1;
            case 5000: return 2;
            case -70000: return 3;
            case Integer.MAX_VALUE: return 4;
            default: return 5;
        }
    }

    static int catching(int[] a, int i) {
        try {
            return a[i] / (i - 3);
        } catch (ArithmeticException e) {
            return -1;
        } catch (ArrayIndexOutOfBoundsException e) {
            return -2;
        } catch (NullPointerException e) {
            return -3;
        }
    }

    static int depth(int n) {
        return n == 0 ? 0 : 1 + depth(n - 1);
    }

    static int overflow(int n) {
        try {
            return overflow(n + 1);
        } catch (StackOverflowError e) {
            return n;
        }
    }

    static long intrinsics(int i) {
        float nan = i < 0 ? 1f : Float.NaN;
        double neg0 = i < 0 ? 1 : -0.0;
        long r = 0;
        r += Float.floatToRawIntBits(Math.max(nan, 1f)) + Float.floatToRawIntBits(Math.min(-0f, 0f));
        r += Double.doubleToRawLongBits(Math.max(neg0, 0.0)) + Double.doubleToRawLongBits(Math.min(neg0, 0.0));
        r += Math.abs(Integer.MIN_VALUE + (i & 1)) + Math.abs(Long.MIN_VALUE + (i & 1)) + (long) Math.abs(-2.5 * i);
        r += Math.max(i, 77) - Math.min(i, -3) + Math.max((long) i << 33, 5L) + Math.min((long) i, -9L);
        r += (long) Math.floor(-i / 3.0) + (long) Math.ceil(i / 7.0) + (long) Math.sqrt(i);
        r += Integer.rotateLeft(i, 13) ^ Integer.rotateRight(i, 37) ^ Long.rotateLeft(i * 31L, 70) ^ Long.rotateRight(i, -3);
        r += Integer.bitCount(i) + Long.bitCount(-i) + Integer.numberOfLeadingZeros(i) + Integer.numberOfTrailingZeros(i);
        r += Long.numberOfLeadingZeros(i) + Long.numberOfTrailingZeros(0L) + Byte.toUnsignedInt((byte) i);
        r += Float.intBitsToFloat(i) == 0 ? 1 : 0;
        r += Double.longBitsToDouble((long) i << 40) > 0 ? 1 : 0;
        try {
            Objects.requireNonNull(i % 100 == 0 ? null : "x");
        } catch (NullPointerException e) {
            r += 1000;
        }
        return r;
    }

    static int casts(Object o) {
        int r = 0;
        if (o instanceof CharSequence) r += 1;
        if (o instanceof Number n) r += n.intValue() & 3;
        try {
            String s = (String) o;
            r += s == null ? 7 : s.length();
        } catch (ClassCastException e) {
            r += 100;
        }
        return r;
    }

    // Inlining: small callees, nested, throwing, allocating, polymorphic.
    static final class Box { int v; Box next; Box(int v) { this.v = v; } int get() { return v; } Box next() { return next; } }
    static int nextValue(Box b) { return b.next().get(); }        // NPE inside inlined code when next is null
    static int twice(Box b) { return nextValue(b) + nextValue(b); } // nested
    static Box make(int v) { return new Box(v); }                  // allocation in inlined code
    static int inlining(int i) {
        Box b = make(i);
        if (i % 3 != 0) b.next = make(i + 1);
        try {
            return twice(b) + b.get();
        } catch (NullPointerException e) {
            return -b.get();
        }
    }

    interface Shape { int area(); }
    record Sq(int s) implements Shape { public int area() { return s * s; } }
    record Rect(int w, int h) implements Shape { public int area() { return w * h; } }
    static final class Tri implements Shape { public int area() { return 3; } }

    public static void main(String[] args) {
        JitPatterns p = new JitPatterns();
        long sum = 0;
        for (int i = 0; i < 20000; i++) {
            p.size = 10;
            Object[] q = new Object[16];
            for (int j = 0; j < 10; j++) q[j] = (j & 1) == 0 ? "x" : null;
            sum += p.shrink(q) + p.shrink(q);
            sum += p.wide(i);
            sum += arith(i * 7919, i - 50);
            sum += sw(i % 9 - 4) + sw(Integer.MIN_VALUE) + lookup(i % 3 == 0 ? 5000 : i);
            sum += catching(i % 5 == 0 ? null : new int[] {1, 2, 3, 4, 5}, i % 7);
            Shape s = switch (i % 3) { case 0 -> new Sq(i & 15); case 1 -> new Rect(i & 7, 3); default -> new Tri(); };
            sum += s.area();
            sum += intrinsics(i);
            sum += inlining(i);
            Object o = switch (i % 4) { case 0 -> "str" + (i & 3); case 1 -> Integer.valueOf(i); case 2 -> new StringBuilder("sb"); default -> null; };
            sum += casts(o);
            counter++;
        }
        System.out.println("sum " + sum + " modCount " + p.modCount + " counter " + counter);
        System.out.println("total " + p.total + " longs " + Arrays.toString(p.longs));
        System.out.println("depth " + depth(3000));
        System.out.println("overflow caught " + (overflow(0) > 1000));
        PriorityQueue<Long> pq = new PriorityQueue<>();
        Random r = new Random(1);
        long acc = 0;
        for (int round = 0; round < 3000; round++) {
            for (int i = 0; i < r.nextInt(20); i++) pq.add(r.nextLong());
            for (Long v : pq) acc += v >>> 60;
            for (int k = 0; k < r.nextInt(5); k++) { Long v = pq.poll(); if (v != null) acc ^= v; }
        }
        System.out.println("pq " + acc + " " + pq.size());
    }
}
