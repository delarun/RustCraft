import java.util.*;
import java.util.concurrent.*;
import java.util.function.*;

/** Allocates heavily from several threads while keeping verifiable live data. */
public class GcStress {
    record Node(int id, String label, Node next, int[] payload) {}

    static long checksum(Node n) {
        long sum = 0;
        for (; n != null; n = n.next) {
            sum = sum * 31 + n.id + n.label.hashCode();
            for (int v : n.payload) sum += v;
        }
        return sum;
    }

    static Node chain(int seed, int length) {
        Node head = null;
        for (int i = 0; i < length; i++) {
            int[] payload = new int[8 + (i % 32)];
            Arrays.fill(payload, seed + i);
            head = new Node(i, "node-" + seed + "-" + i, head, payload);
        }
        return head;
    }

    public static void main(String[] args) throws Exception {
        // Long-lived data that must survive many collections.
        List<Node> keep = new ArrayList<>();
        for (int i = 0; i < 50; i++) keep.add(chain(i, 200));
        long expected = 0;
        for (Node n : keep) expected += checksum(n);

        ExecutorService pool = Executors.newFixedThreadPool(4);
        List<Future<Long>> results = new ArrayList<>();
        for (int t = 0; t < 8; t++) {
            final int seed = t;
            results.add(pool.submit(() -> {
                long total = 0;
                Map<String, List<Integer>> map = new HashMap<>();
                for (int round = 0; round < 3000; round++) {
                    Node garbage = chain(seed * 1000 + round, 50);
                    total += checksum(garbage) & 0xFFFF;
                    String key = "k" + (round % 37);
                    map.computeIfAbsent(key, k -> new ArrayList<>()).add(round);
                    StringBuilder sb = new StringBuilder();
                    for (int i = 0; i < 20; i++) sb.append(i).append(',');
                    total += sb.toString().length();
                    Function<Integer, String> f = x -> "v" + x + seed;
                    total += f.apply(round).length();
                }
                for (List<Integer> l : map.values()) total += l.size();
                return total;
            }));
        }
        long sum = 0;
        for (Future<Long> f : results) sum += f.get();
        pool.shutdown();

        long after = 0;
        for (Node n : keep) after += checksum(n);
        System.out.println("worker total = " + sum);
        System.out.println("long-lived intact = " + (after == expected));
        System.out.println("checksum = " + after);
    }
}
