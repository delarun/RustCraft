import java.lang.invoke.MethodHandles;
import java.lang.invoke.VarHandle;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import vmtest.Out;

// Unaligned accesses in both byte orders (Unsafe.get/put*Unaligned under
// VarHandles and ByteBuffers), compared with HotSpot.
public class Unaligned {
    static final VarHandle LONG_BE = MethodHandles.byteArrayViewVarHandle(long[].class, ByteOrder.BIG_ENDIAN);
    static final VarHandle LONG_LE = MethodHandles.byteArrayViewVarHandle(long[].class, ByteOrder.LITTLE_ENDIAN);
    static final VarHandle INT_BE = MethodHandles.byteArrayViewVarHandle(int[].class, ByteOrder.BIG_ENDIAN);
    static final VarHandle INT_LE = MethodHandles.byteArrayViewVarHandle(int[].class, ByteOrder.LITTLE_ENDIAN);
    static final VarHandle SHORT_BE = MethodHandles.byteArrayViewVarHandle(short[].class, ByteOrder.BIG_ENDIAN);
    static final VarHandle CHAR_LE = MethodHandles.byteArrayViewVarHandle(char[].class, ByteOrder.LITTLE_ENDIAN);

    static long round(byte[] b, ByteBuffer heapBe, ByteBuffer directLe, int i) {
        long x = 0x0102030405060708L * (i + 1) ^ i;
        for (int at = 0; at + 8 <= b.length; at += 3) {
            LONG_BE.set(b, at, x);
            x = x * 31 + (long) LONG_LE.get(b, at) + (long) LONG_BE.get(b, at);
            INT_BE.set(b, at + 1, (int) x);
            x = x * 31 + (int) INT_LE.get(b, at) + (int) INT_BE.get(b, at + 1);
            SHORT_BE.set(b, at + 2, (short) (x >> 7));
            x = x * 31 + (short) SHORT_BE.get(b, at + 2) + (char) CHAR_LE.get(b, at + 1);
            CHAR_LE.set(b, at + 3, (char) (x >> 3));
            x = x * 31 + (short) SHORT_BE.get(b, at + 3);
        }
        for (int at = 0; at + 8 <= 64; at += 5) {
            heapBe.putLong(at, x).putInt(at + 1, (int) x ^ 0x80000000).putShort(at + 2, (short) -2).putChar(at + 5, '￾');
            directLe.putLong(at, x).putInt(at + 3, (int) (x >>> 32)).putShort(at, (short) 0x8001);
            x = x * 31 + heapBe.getLong(at) + heapBe.getInt(at + 1) + heapBe.getShort(at + 2) + heapBe.getChar(at + 5)
                + directLe.getLong(at) + directLe.getInt(at + 3) + directLe.getShort(at) + directLe.getChar(at + 1);
        }
        return x;
    }

    public static void main(String[] args) {
        byte[] b = new byte[64];
        ByteBuffer heapBe = ByteBuffer.allocate(64).order(ByteOrder.BIG_ENDIAN);
        ByteBuffer directLe = ByteBuffer.allocateDirect(64).order(ByteOrder.LITTLE_ENDIAN);
        long check = 0;
        for (int i = 0; i < 3000; i++) {
            check = check * 31 + round(b, heapBe, directLe, i);
            if (i == 0 || i == 2999) {
                StringBuilder s = new StringBuilder();
                for (byte x : b) s.append(Integer.toHexString(x & 0xFF)).append(' ');
                Out.println(s.toString().trim());
                Out.println(check);
            }
        }
    }
}
