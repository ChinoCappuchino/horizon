package t;

// Test input for the compiler (tests/exceptions.rs): throwing and catching,
// finally, synchronized, and arrays of arrays.
public class Errors {
    static class MyError extends RuntimeException {
        int code;

        MyError(int code) {
            super("my");
            this.code = code;
        }
    }

    static int counter;

    static int risky(int x) {
        if (x < 0) throw new MyError(x);
        if (x == 0) throw new IllegalStateException("zero");
        return x;
    }

    public static int catchMine(int x) {
        try {
            return risky(x);
        } catch (MyError e) {
            return e.code * 10;
        }
    }

    public static int catchEither(int x) {
        try {
            return risky(x);
        } catch (MyError e) {
            return -1;
        } catch (RuntimeException e) {
            return -2;
        }
    }

    public static int withFinally(int x) {
        int n = 0;
        try {
            n += risky(x);
        } catch (RuntimeException e) {
            n += 100;
        } finally {
            n += 1000;
        }
        return n;
    }

    public static int nested(int x) {
        counter = 0;
        try {
            try {
                return risky(x);
            } finally {
                counter++;
            }
        } catch (MyError e) {
            return counter * 7;
        }
    }

    public static int locked(Object o) {
        synchronized (o) {
            return 5;
        }
    }

    public static int grid() {
        int[][] g = new int[3][4];
        g[2][3] = 7;
        long[][][] cube = new long[2][2][];
        return g.length * 100 + g[0].length * 10 + g[2][3] + (cube[1][1] == null ? 1000 : 0);
    }
}
