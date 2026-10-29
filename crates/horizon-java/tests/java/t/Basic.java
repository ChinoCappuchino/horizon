package t;

// Test input for the compiler (tests/compile.rs): arithmetic, locals,
// branches, switches, static fields and initializers, static calls.
public class Basic {
    static final int BASE = 10;
    static int counter;
    static long big = 1L << 40;

    static {
        counter = 5;
    }

    public static int add(int a, int b) { return a + b; }

    public static int sum(int n) {
        int s = 0;
        for (int i = 0; i < n; i++) s += i;
        return s;
    }

    public static int max(int a, int b) { return a > b ? a : b; }

    public static int next() { return ++counter + BASE; }

    public static int bump() { return counter++; }

    public static int pick(int k) {
        switch (k) {
            case 1: return 10;
            case 2: return 20;
            case 3: return 30;
            case 100: return 1000;
            default: return -1;
        }
    }

    public static long fact(int n) {
        long r = 1;
        while (n > 1) r *= n--;
        return r;
    }

    public static int div(int a, int b) { return a / b; }

    public static int rem(int a, int b) { return a % b; }

    public static int order(float a, float b) { return a < b ? -1 : (a > b ? 1 : 0); }

    public static double mod(double a, double b) { return a % b; }

    public static int toInt(double d) { return (int) d; }

    public static long shifted() { return big >>> 3; }

    public static int fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

    public static long twice(long x) {
        long y;
        long z = y = x + 1;
        return z + y;
    }

    public static int nextChar(char c) { return (char) (c + 1); }

    public static int toByte(int x) { return (byte) x; }
}
