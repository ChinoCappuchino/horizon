package t;

import java.util.function.IntUnaryOperator;

/** Cases the native backend once stopped on (tests/native.rs). */
public class NativeOps {
    /** `a[i]++` on a long array: dup2_x2 with a wide value over two narrow ones. */
    public static long postIncLong(long[] a, int i) {
        long before = a[i]++;
        return before * 1000 + a[i];
    }

    public static double postIncDouble(double[] a, int i) {
        double before = a[i]++;
        return before * 1000 + a[i];
    }

    public static long postIncLongs(int i) {
        long[] a = {10, 20, 30};
        return postIncLong(a, i) + a[i];
    }

    public static double postIncDoubles(int i) {
        double[] a = {1.5, 2.5, 3.5};
        return postIncDouble(a, i) + a[i];
    }

    /** A lambda in an abstract class: its body is a private instance method. */
    abstract static class Shape {
        int k;

        abstract int size();

        IntUnaryOperator op() {
            return y -> y * k + size();
        }
    }

    static class One extends Shape {
        int size() {
            return 1;
        }
    }

    static class Two extends Shape {
        int size() {
            return 2;
        }
    }

    public static int lambdaInAbstract(int x) {
        Shape a = new One();
        a.k = 3;
        Shape b = new Two();
        b.k = 5;
        return a.op().applyAsInt(x) * 100 + b.op().applyAsInt(x);
    }

    /** Allocates all the way: arrays of varying length, a list of nodes. */
    static final class Node {
        final int value;
        final Node next;

        Node(int value, Node next) {
            this.value = value;
            this.next = next;
        }
    }

    public static long churn(int n) {
        long sum = 0;
        Node head = null;
        for (int i = 0; i < n; i++) {
            int[] a = new int[8 + (i & 63)];
            a[i % a.length] = i;
            sum += a.length + a[i % a.length];
            head = new Node(i, (i & 1023) == 0 ? null : head);
        }
        for (Node x = head; x != null; x = x.next) {
            sum += x.value;
        }
        return sum;
    }

    /** multianewarray: 2-D ints, 3-D chars, an outer count of 0. */
    public static int multi(int a, int b) {
        int[][] m = new int[a][b];
        m[a - 1][b - 1] = 7;
        char[][][] c = new char[2][3][4];
        c[1][2][3] = 'z';
        long[][] none = new long[0][5];
        return m.length * 1000 + m[0].length * 100 + m[a - 1][b - 1] + c[1][2][3] + none.length
                + c[0][0].length * 10000;
    }

    /** A lambda in an interface's default method calling a private interface method. */
    interface Greeter {
        String name();

        default java.util.function.Supplier<String> greet() {
            return () -> hello();
        }

        private String hello() {
            return "hi " + name();
        }
    }

    public static int interfaceLambda() {
        Greeter g = () -> "bo";
        return g.greet().get().length();
    }

    /** Two lambda sites at the same offset of two methods (offset 1 in each). */
    static java.util.function.Supplier<String> supplierAt1(int x) {
        return () -> "s" + x;
    }

    static java.util.function.Function<Integer, Integer> functionAt1(int x) {
        return y -> y + x;
    }

    public static int sameOffsetLambdas() {
        return functionAt1(2).apply(3) * 10 + supplierAt1(4).get().length();
    }

    /** instanceof on lambdas of the runtime's interfaces. */
    public static int lambdaInstanceOf() {
        Object f = (java.util.function.Function<Integer, Integer>) x -> x + 1;
        Object c = (java.util.Comparator<Integer>) (a, b) -> a - b;
        int r = 0;
        if (f instanceof java.util.function.Function) r |= 1;
        if (!(f instanceof java.util.Comparator)) r |= 2;
        if (c instanceof java.util.Comparator) r |= 4;
        if (!(c instanceof java.util.function.Function)) r |= 8;
        return r;
    }
}
