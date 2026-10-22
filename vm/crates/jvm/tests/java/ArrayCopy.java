import java.util.Arrays;
import vmtest.Out;

// System.arraycopy: every element kind, overlap, and the failures (which
// compiled code leaves to the VM), compared with HotSpot.
public class ArrayCopy {
    static String attempt(Object src, int sp, Object dst, int dp, int n) {
        try {
            System.arraycopy(src, sp, dst, dp, n);
            return "ok";
        } catch (RuntimeException e) {
            return e.getClass().getSimpleName();
        }
    }

    public static void main(String[] args) {
        long check = 0;
        for (int round = 0; round < 2000; round++) {
            int[] ints = new int[16];
            for (int i = 0; i < ints.length; i++) ints[i] = i * round;
            System.arraycopy(ints, 0, ints, 3, 10); // overlapping, forwards
            System.arraycopy(ints, 5, ints, 1, 8); // overlapping, backwards
            long[] longs = new long[8];
            System.arraycopy(new long[] {round, -1L, Long.MIN_VALUE}, 0, longs, 4, 3);
            byte[] bytes = "abcdefghij".getBytes();
            System.arraycopy(bytes, 2, bytes, 0, 7);
            char[] chars = new char[5];
            System.arraycopy("hello".toCharArray(), 1, chars, 0, 4);
            double[] doubles = {1.5, 2.5};
            double[] doubles2 = new double[3];
            System.arraycopy(doubles, 0, doubles2, 1, 2);
            String[] strings = {"a", "b", "c"};
            Object[] objects = new Object[4];
            System.arraycopy(strings, 0, objects, 1, 3); // String[] into Object[]
            Object[] mixed = {"x", 1, "y"};
            String[] into = new String[3];
            String failed = attempt(mixed, 0, into, 0, 3); // element check fails at 1
            check = check * 31 + Arrays.hashCode(ints) + Arrays.hashCode(longs) + Arrays.hashCode(bytes) + Arrays.hashCode(chars)
                + Arrays.hashCode(doubles2) + Arrays.hashCode(objects) + Arrays.hashCode(into) + failed.hashCode();
            if (round == 0) {
                Out.println(Arrays.toString(ints) + " " + Arrays.toString(longs) + " " + new String(bytes) + " " + new String(chars));
                Out.println(Arrays.toString(doubles2) + " " + Arrays.toString(objects) + " " + Arrays.toString(into) + " " + failed);
                Out.println(attempt(null, 0, ints, 0, 1) + " " + attempt(ints, 0, null, 0, 1));
                Out.println(attempt(ints, -1, ints, 0, 1) + " " + attempt(ints, 0, ints, 0, -1) + " " + attempt(ints, 10, ints, 0, 7));
                Out.println(attempt(ints, 0, longs, 0, 1) + " " + attempt("s", 0, ints, 0, 1) + " " + attempt(objects, 0, strings, 0, 1));
                Out.println(attempt(ints, 16, ints, 0, 0) + " " + attempt(ints, 17, ints, 0, 0) + " " + attempt(ints, Integer.MAX_VALUE, ints, 0, 1));
            }
        }
        Out.println(check);
    }
}
