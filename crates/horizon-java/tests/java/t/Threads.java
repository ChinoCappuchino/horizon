package t;

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
}
