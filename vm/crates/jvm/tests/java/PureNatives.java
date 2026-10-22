import java.util.concurrent.atomic.*;
import vmtest.Out;

// Natives the JIT answers without a full call, run hot; output compared with HotSpot.
public class PureNatives {
    static class Plain {}
    static class Hashed { public int hashCode() { return 42; } }
    record Rec(int a) {}
    public static void main(String[] args) throws Exception {
        Object[] objs = { new Plain(), new Hashed(), "str", new Rec(3), new int[2], null };
        long sum = 0;
        int sameThread = 0, nullErrors = 0, overriddenHash = 0, identityStable = 0;
        Object plain = objs[0];
        int plainHash = plain.hashCode();
        AtomicInteger ai = new AtomicInteger();
        AtomicLong al = new AtomicLong();
        AtomicReference<String> ar = new AtomicReference<>("a");
        AtomicIntegerArray arr = new AtomicIntegerArray(8);
        for (int i = 0; i < 300_000; i++) {
            Object o = objs[i % objs.length];
            if (Thread.currentThread() == Thread.currentThread()) sameThread++;
            try {
                sum += o.getClass().getName().length();
            } catch (NullPointerException e) { nullErrors++; }
            if (o != null) {
                int h = o.hashCode();
                if (o instanceof Hashed && h == 42) overriddenHash++;
                if (o == plain && h == plainHash && System.identityHashCode(o) == plainHash) identityStable++;
            }
            sum += System.identityHashCode(null);
            sum += CharSequence.class.isInstance(o) ? 1 : 0;
            sum += Object.class.isInstance(o) ? 2 : 0;
            sum += Number.class.isInstance(o) ? 4 : 0;
            try { ((Class<?>) (i % 1000 == 0 ? null : Rec.class)).isInstance(o); } catch (NullPointerException e) { nullErrors++; }
            ai.compareAndSet(i - 1, i);
            if (!al.compareAndSet((long) i << 33, ((long) i + 1) << 33)) sum += 1000;
            ar.compareAndSet(i % 2 == 0 ? "a" : "b", i % 2 == 0 ? "b" : "a");
            arr.compareAndSet(i & 7, i - 8 >= 0 ? i - 8 : 0, i);
            ai.incrementAndGet(); ai.decrementAndGet();
        }
        Out.println("sum " + sum + " same " + sameThread + " npe " + nullErrors + " overridden " + overriddenHash
            + " stable " + identityStable + " ai " + ai.get() + " al " + (al.get() >> 33) + " ar " + ar.get() + " arr " + arr);
    }
}
