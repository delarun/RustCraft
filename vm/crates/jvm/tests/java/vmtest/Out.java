package vmtest;

/** Output for VM tests. On our VM println(String) is replaced by a native hook,
 *  so tests can print before System.out is initialised. */
public final class Out {
    private Out() {}

    public static void println(String s) { System.out.println(s); }
    public static void println(Object o) { println(String.valueOf(o)); }
    public static void println(int i) { println(Integer.toString(i)); }
    public static void println(long l) { println(Long.toString(l)); }
    public static void println(boolean b) { println(b ? "true" : "false"); }
    public static void println(char c) { println(String.valueOf(c)); }
    public static void println(double d) { println(Double.toString(d)); }
    public static void println(float f) { println(Float.toString(f)); }
}
