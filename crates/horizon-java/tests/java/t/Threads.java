package t;

import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ForkJoinPool;
import java.util.concurrent.ForkJoinWorkerThread;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/** Threads, monitors, atomics and class initialization (tests/native.rs). */
public class Threads {
    static final class Counter {
        int n;

        synchronized void add() {
            n++;
        }
    }

    public static int synchronizedCounter(int threads, int per) throws InterruptedException {
        Counter c = new Counter();
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            ts[t] = new Thread(() -> {
                for (int i = 0; i < per; i++) c.add();
            });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
        return c.n;
    }

    static final Object LOCK = new Object();
    static int locked;

    public static int synchronizedBlocks(int threads, int per) throws InterruptedException {
        locked = 0;
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            ts[t] = new Thread(() -> {
                for (int i = 0; i < per; i++) {
                    synchronized (LOCK) {
                        locked++;
                    }
                }
            });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
        return locked;
    }

    public static int atomics(int threads, int per) throws InterruptedException {
        AtomicInteger a = new AtomicInteger();
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            ts[t] = new Thread(() -> {
                for (int i = 0; i < per; i++) a.incrementAndGet();
            });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
        return a.get();
    }

    static final class Worker extends Thread {
        final int upTo;
        long sum;

        Worker(int upTo) {
            this.upTo = upTo;
        }

        @Override
        public void run() {
            for (int i = 1; i <= upTo; i++) sum += i;
        }
    }

    public static long subclass(int upTo) throws InterruptedException {
        Worker w = new Worker(upTo);
        w.start();
        w.join();
        return w.sum;
    }

    static final class Slot {
        int value;
        boolean full;

        synchronized void put(int v) throws InterruptedException {
            while (full) wait();
            value = v;
            full = true;
            notifyAll();
        }

        synchronized int take() throws InterruptedException {
            while (!full) wait();
            full = false;
            notifyAll();
            return value;
        }
    }

    public static long handoff(int n) throws InterruptedException {
        Slot s = new Slot();
        Thread producer = new Thread(() -> {
            try {
                for (int i = 1; i <= n; i++) s.put(i);
            } catch (InterruptedException e) {
                // Not interrupted here.
            }
        });
        producer.start();
        long sum = 0;
        for (int i = 0; i < n; i++) sum += s.take();
        producer.join();
        return sum;
    }

    static final class Slow {
        static final int VALUE;
        static int inits;

        static {
            inits++;
            try {
                Thread.sleep(50);
            } catch (InterruptedException e) {
                // Not interrupted here.
            }
            VALUE = 42;
        }
    }

    /** Every thread sees the class initialized once: threads * 42 + 1000. */
    public static int initRace(int threads) throws InterruptedException {
        int[] seen = new int[threads];
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            int k = t;
            ts[t] = new Thread(() -> seen[k] = Slow.VALUE);
        }
        for (Thread t : ts) t.start();
        for (Thread t : ts) t.join();
        int sum = 0;
        for (int v : seen) sum += v;
        return sum + Slow.inits * 1000;
    }

    static final AtomicInteger STARTS = new AtomicInteger();
    static final AtomicInteger ENDS = new AtomicInteger();

    static final class PoolWorker extends ForkJoinWorkerThread {
        PoolWorker(ForkJoinPool pool) {
            super(pool);
        }

        @Override
        protected void onStart() {
            STARTS.incrementAndGet();
        }

        @Override
        protected void onTermination(Throwable e) {
            ENDS.incrementAndGet();
        }
    }

    /** tasks * 1e6 + submitted value * 1e4 + starts * 100 + ends * 10 + ended in time. */
    public static long pool(int workers, int tasks) throws Exception {
        STARTS.set(0);
        ENDS.set(0);
        AtomicInteger done = new AtomicInteger();
        ForkJoinPool pool = new ForkJoinPool(workers, PoolWorker::new, null, true);
        for (int i = 0; i < tasks; i++) pool.execute(() -> done.incrementAndGet());
        Future<Integer> seven = pool.submit(() -> 7);
        int value = seven.get();
        pool.shutdown();
        boolean ended = pool.awaitTermination(10, TimeUnit.SECONDS);
        return done.get() * 1_000_000L + value * 10_000L + STARTS.get() * 100L + ENDS.get() * 10L
                + (ended ? 1 : 0);
    }

    /** done * 1000 + cached pool's threads in 1..tasks * 100 + fixed pool's <= 3 * 10 + both ended. */
    public static long executors(int tasks) throws Exception {
        AtomicInteger done = new AtomicInteger();
        AtomicInteger made = new AtomicInteger();
        ExecutorService cached = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r, "io-" + made.incrementAndGet());
            t.setDaemon(true);
            return t;
        });
        for (int i = 0; i < tasks; i++) cached.execute(done::incrementAndGet);
        cached.shutdown();
        boolean first = cached.awaitTermination(10, TimeUnit.SECONDS);
        AtomicInteger fixedMade = new AtomicInteger();
        ExecutorService fixed = Executors.newFixedThreadPool(3, r -> {
            fixedMade.incrementAndGet();
            return new Thread(r);
        });
        for (int i = 0; i < tasks; i++) fixed.execute(done::incrementAndGet);
        fixed.shutdown();
        boolean second = fixed.awaitTermination(10, TimeUnit.SECONDS);
        int threadsOk = made.get() >= 1 && made.get() <= tasks ? 1 : 0;
        int fixedOk = fixedMade.get() >= 1 && fixedMade.get() <= 3 ? 1 : 0;
        return done.get() * 1000L + threadsOk * 100 + fixedOk * 10 + (first && second ? 1 : 0);
    }

    /** Type tests and catch clauses on another thread: each check one bit. */
    public static int typesOnThreads() throws InterruptedException {
        int[] out = new int[1];
        Thread t = new Thread(() -> {
            Object counter = new Counter();
            Object text = "text";
            int r = 0;
            if (counter instanceof Counter) r |= 1;
            if (!(text instanceof Counter)) r |= 2;
            try {
                throw new IllegalStateException("x");
            } catch (IllegalStateException e) {
                r |= 4;
            }
            try {
                Counter bad = (Counter) text;
                r |= bad.n;
            } catch (ClassCastException e) {
                r |= 8;
            }
            out[0] = r;
        });
        t.start();
        t.join();
        return out[0];
    }
}
