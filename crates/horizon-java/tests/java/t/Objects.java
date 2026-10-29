package t;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Function;
import java.util.function.IntUnaryOperator;
import java.util.function.Supplier;

// Test input for the compiler (tests/runtime.rs): objects of the jar's
// classes, the host's objects, strings, and lambdas.
public class Objects {
    int count;
    long total;
    String name;

    Objects(String name) {
        this.name = name;
    }

    void add(int n) {
        count++;
        total += n;
    }

    int count() { return count; }

    public static int make() {
        Objects o = new Objects("x");
        o.add(3);
        o.add(4);
        return o.count() * 100 + (int) o.total;
    }

    public static String greet() { return "hello"; }

    public static String nameOf() { return new Objects("named").name; }

    public static List<String> list() {
        List<String> l = new ArrayList<>();
        l.add("a");
        l.add(greet());
        return l;
    }

    static int applyTwice(IntUnaryOperator f, int x) { return f.applyAsInt(f.applyAsInt(x)); }

    public static int useLambda(int k) { return applyTwice(x -> x * k, 3); }

    public static Function<Integer, Integer> boxed() { return x -> x + 1; }

    static int sevenValue() { return 7; }

    public static Supplier<Integer> seven() { return Objects::sevenValue; }

    public static Supplier<String> exclaim(String s) { return () -> s.concat("!"); }

    public static Supplier<Objects> maker() { return () -> new Objects("made"); }

    public static String madeName(Supplier<Objects> s) { return s.get().name; }

    public static boolean same(Object a, Object b) { return a == b; }
}
