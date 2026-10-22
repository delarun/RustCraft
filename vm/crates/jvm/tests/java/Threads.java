import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;

public class Threads {
    static int counter;

    static synchronized void increment() { counter++; }

    public static void main(String[] args) throws Exception {
        // Synchronized counter across threads.
        Thread[] workers = new Thread[4];
        for (int i = 0; i < workers.length; i++) {
            workers[i] = new Thread(() -> { for (int j = 0; j < 20000; j++) increment(); }, "worker-" + i);
            workers[i].start();
        }
        for (Thread w : workers) w.join();
        System.out.println("counter = " + counter);

        // wait/notify producer-consumer.
        LinkedList<Integer> queue = new LinkedList<>();
        Thread producer = new Thread(() -> {
            for (int i = 1; i <= 5; i++) {
                synchronized (queue) {
                    queue.add(i);
                    queue.notifyAll();
                }
            }
            synchronized (queue) { queue.add(-1); queue.notifyAll(); }
        });
        producer.start();
        int sum = 0;
        while (true) {
            int item;
            synchronized (queue) {
                while (queue.isEmpty()) queue.wait();
                item = queue.removeFirst();
            }
            if (item < 0) break;
            sum += item;
        }
        producer.join();
        System.out.println("consumed sum = " + sum);

        // Executors, futures, latches, atomics, concurrent maps.
        ExecutorService pool = Executors.newFixedThreadPool(3);
        AtomicInteger atomic = new AtomicInteger();
        ConcurrentHashMap<String, Integer> map = new ConcurrentHashMap<>();
        CountDownLatch latch = new CountDownLatch(10);
        List<Future<Integer>> futures = new ArrayList<>();
        for (int i = 0; i < 10; i++) {
            final int n = i;
            futures.add(pool.submit(() -> {
                atomic.addAndGet(n);
                map.merge(n % 2 == 0 ? "even" : "odd", 1, Integer::sum);
                latch.countDown();
                return n * n;
            }));
        }
        latch.await();
        int squares = 0;
        for (Future<Integer> f : futures) squares += f.get();
        pool.shutdown();
        System.out.println("terminated = " + pool.awaitTermination(5, TimeUnit.SECONDS));
        System.out.println("atomic = " + atomic.get() + ", squares = " + squares + ", map = " + new TreeMap<>(map));

        // Interrupting a sleeping thread.
        Thread sleeper = new Thread(() -> {
            try {
                Thread.sleep(60_000);
                System.out.println("woke normally");
            } catch (InterruptedException e) {
                System.out.println("sleeper interrupted");
            }
        });
        sleeper.start();
        Thread.sleep(50);
        sleeper.interrupt();
        sleeper.join();
        System.out.println("sleeper state = " + sleeper.getState());

        // CompletableFuture chain.
        String result = CompletableFuture.supplyAsync(() -> "async")
            .thenApply(s -> s + "-mapped")
            .thenCombine(CompletableFuture.completedFuture(42), (s, n) -> s + "-" + n)
            .get(5, TimeUnit.SECONDS);
        System.out.println(result);

        // A daemon thread does not keep the VM alive.
        Thread daemon = new Thread(() -> { while (true) { try { Thread.sleep(1000); } catch (InterruptedException e) { return; } } });
        daemon.setDaemon(true);
        daemon.start();
        Runtime.getRuntime().addShutdownHook(new Thread(() -> System.out.println("shutdown hook ran")));
        System.out.println("main done");
    }
}
