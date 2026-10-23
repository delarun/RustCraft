import java.lang.reflect.Array;
import java.net.URL;
import java.net.URLClassLoader;
import vmtest.Out;

// java.lang.reflect.Array accessors (boxing, widening, failures), array
// classes by name through a user class loader, and code sources.
public class ReflectArrays {
    static String attempt(Runnable r) {
        try {
            r.run();
            return "ok";
        } catch (RuntimeException e) {
            return e.getClass().getSimpleName();
        }
    }

    public static void main(String[] args) throws Exception {
        int[] ints = {1, -2, 3};
        byte[] bytes = {-5, 7};
        char[] chars = {'a', 'Z'};
        double[] doubles = new double[2];
        boolean[] bools = new boolean[2];
        String[] strings = {"x", null};
        Object[] objects = new Object[2];
        Out.println(Array.get(ints, 1) + " " + Array.get(bytes, 0) + " " + Array.get(chars, 1) + " " + Array.get(strings, 0));
        Out.println(Array.getInt(bytes, 0) + " " + Array.getLong(ints, 1) + " " + Array.getDouble(chars, 0) + " " + Array.getFloat(ints, 2));
        Array.set(ints, 0, 42);
        Array.set(doubles, 0, 1.5f);
        Array.setInt(doubles, 1, 7);
        Array.setBoolean(bools, 1, true);
        Array.set(strings, 1, "y");
        Array.set(objects, 0, 12L);
        Array.setByte(ints, 2, (byte) -1);
        Out.println(ints[0] + " " + ints[2] + " " + doubles[0] + " " + doubles[1] + " " + bools[1] + " " + strings[1] + " " + objects[0]);
        Out.println(attempt(() -> Array.set(strings, 0, 3)) + " " + attempt(() -> Array.getInt(doubles, 0)) + " " + attempt(() -> Array.setLong(ints, 0, 1L)));
        Out.println(attempt(() -> Array.get(ints, 3)) + " " + attempt(() -> Array.get("no", 0)) + " " + attempt(() -> Array.set(ints, 0, null)));
        Out.println(attempt(() -> Array.getInt(strings, 0)) + " " + attempt(() -> Array.setBoolean(ints, 0, true)) + " " + attempt(() -> Array.set(ints, 0, "s")));
        long check = 0;
        for (int i = 0; i < 20000; i++) {
            Array.setInt(ints, i % 3, i);
            check += Array.getLong(ints, (i + 1) % 3) + (int) Array.get(ints, i % 3);
        }
        Out.println(check);

        // Arrays by name through a user loader (made by the VM; the loader only
        // resolves the element type), as dynamic proxies do.
        ClassLoader loader = new URLClassLoader(new URL[0], ReflectArrays.class.getClassLoader());
        Out.println(Class.forName("[Ljava.lang.Object;", false, loader).getName() + " "
            + Class.forName("[[I", false, loader).getName() + " "
            + Class.forName("[LReflectArrays;", false, loader).getComponentType().getName());
        try {
            Class.forName("[LNoSuchClass;", false, loader);
        } catch (ClassNotFoundException e) {
            Out.println("CNFE " + e.getMessage());
        }
        // Classes from the class path have a code source (a directory URL).
        URL location = ReflectArrays.class.getProtectionDomain().getCodeSource().getLocation();
        Out.println(location.getProtocol() + " " + location.getPath().endsWith("/"));
    }
}
