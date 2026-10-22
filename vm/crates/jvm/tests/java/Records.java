import java.util.*;
import vmtest.Out;

// Record equals/hashCode on every component type, compared with HotSpot.
public class Records {
    record Empty() {}
    record Ints(int i, short s, char c, byte b, boolean z) {}
    record Wide(long l, double d, float f) {}
    record Refs(String s, Object o, Integer a, List<String> list) {}
    record Mixed(boolean z, long l, String s, float f, Empty e) {}
    record Generic<T>(T value, int n) {}
    public static void main(String[] args) {
        int[] arr = {1, 2};
        Object[] values = {
            new Empty(), new Empty(),
            new Ints(1, (short) -2, 'x', (byte) 7, true), new Ints(1, (short) -2, 'x', (byte) 7, true), new Ints(1, (short) -2, 'x', (byte) 7, false),
            new Wide(1L << 40, -0.0, Float.NaN), new Wide(1L << 40, 0.0, Float.NaN), new Wide(1L << 40, -0.0, Float.NaN), new Wide(-1, Double.NaN, -0.0f),
            new Refs("a", null, 1000, List.of("q")), new Refs("a", null, 1000, List.of("q")), new Refs("a", null, 1001, List.of("q")), new Refs(null, 3, null, null),
            new Mixed(true, 5, "m", 1.5f, new Empty()), new Mixed(true, 5, "m", 1.5f, new Empty()), new Mixed(false, 5, "m", 1.5f, null),
            new Generic<>("g", 1), new Generic<>("g", 1), new Generic<>(1, 1), "not a record", null,
        };
        StringBuilder out = new StringBuilder();
        long check = 0;
        for (int round = 0; round < 3000; round++) {
            for (int i = 0; i < values.length; i++) {
                Object a = values[i];
                if (a == null) continue;
                if (round == 0) out.append(a.getClass().getSimpleName()).append(' ').append(a.hashCode()).append('\n');
                check = check * 31 + a.hashCode();
                for (Object b : values) {
                    boolean eq = a.equals(b);
                    check = check * 7 + (eq ? 1 : 0);
                    if (round == 0 && eq && a != b) out.append("  equal: ").append(a).append(" == ").append(b).append('\n');
                }
            }
        }
        Out.println(out.toString().stripTrailing());
        Out.println("check " + check);
        Map<Object, Integer> map = new HashMap<>();
        for (Object v : values) if (v != null) map.merge(v, 1, Integer::sum);
        Out.println("map " + map.values());
    }
}
