package t;

import java.util.ArrayList;
import java.util.BitSet;
import java.util.Arrays;
import java.util.Collections;
import java.util.Comparator;
import java.util.function.Function;
import java.util.function.Predicate;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.stream.Collectors;
import java.util.stream.Stream;

/** Streams and Collectors: each check one bit (tests/native.rs). */
public class Streams {
    public static int checks() {
        int bits = 0;
        List<Integer> xs = new ArrayList<>();
        for (int i = 20; i > 0; i--) xs.add(i);
        List<Integer> out = xs.stream()
                .filter(x -> x % 2 == 0)
                .map(x -> x * 3)
                .sorted()
                .limit(4)
                .collect(Collectors.toList());
        int sum = 0;
        for (int v : out) sum += v;
        if (sum == 6 + 12 + 18 + 24 && out.size() == 4) bits |= 1;

        if (Stream.iterate(1, x -> x * 2).limit(10).reduce(0, Integer::sum) == 1023) bits |= 2;

        String joined = Stream.of("b", "a", "c", "a")
                .distinct()
                .sorted()
                .collect(Collectors.joining(",", "[", "]"));
        if (joined.equals("[a,b,c]")) bits |= 4;

        List<Integer> small = List.of(1, 2, 3);
        if (small.stream().anyMatch(x -> x > 2)
                && small.stream().allMatch(x -> x > 0)
                && small.stream().noneMatch(x -> x > 5)
                && !small.stream().anyMatch(x -> x > 3)) bits |= 8;

        List<Integer> ten = new ArrayList<>();
        for (int i = 0; i < 10; i++) ten.add(i);
        Map<Integer, Long> byRest = ten.stream()
                .collect(Collectors.groupingBy(x -> x % 3, Collectors.counting()));
        if (byRest.get(0) == 4L && byRest.get(1) == 3L && byRest.get(2) == 3L) bits |= 16;

        Map<Integer, String> byLength = Stream.of("aa", "b", "cc", "d")
                .collect(Collectors.toMap(String::length, s -> s, (a, b) -> a + b));
        if (byLength.get(2).equals("aacc") && byLength.get(1).equals("bd")) bits |= 32;

        if (!Stream.empty().findFirst().isPresent()
                && small.stream().filter(x -> x > 2).findFirst().get() == 3) bits |= 64;

        if (Stream.of(List.of(1, 2), List.of(3)).flatMap(List::stream).count() == 3) bits |= 128;

        if (Stream.of(1, 2, 3, 4).skip(1).count() == 3) bits |= 256;

        AtomicInteger seen = new AtomicInteger();
        int five = Stream.iterate(0, x -> x + 1)
                .peek(x -> seen.incrementAndGet())
                .filter(x -> x > 4)
                .findFirst()
                .get();
        if (five == 5 && seen.get() == 6) bits |= 512;

        if (Arrays.stream(new String[] {"x", "yyy", "yy"})
                .map(String::length)
                .max(Integer::compare)
                .get() == 3) bits |= 1024;

        if (Stream.of(3, 1, 2).sorted((a, b) -> b - a).toList().get(0) == 3) bits |= 2048;
        return bits;
    }

    /** Collections' algorithms: each check one bit. */
    public static int collectionsChecks() {
        int bits = 0;
        List<Character> letters = new ArrayList<>(List.of('c', 'a', 'z', 'b'));
        if (Collections.max(letters) == 'z' && Collections.min(letters) == 'a') bits |= 1;
        if (Collections.min(letters, (a, b) -> b - a) == 'z') bits |= 2;
        List<String> copies = Collections.nCopies(3, "x");
        if (copies.size() == 3 && copies.get(2).equals("x")) bits |= 4;
        Collections.reverse(letters);
        if (letters.get(0) == 'b' && letters.get(3) == 'c') bits |= 8;
        if (Collections.frequency(List.of(1, 2, 1, 1), 1) == 3) bits |= 16;
        if (Collections.singletonList(5).get(0) == 5
                && Collections.singleton("s").contains("s")
                && Collections.singletonMap("k", 9).get("k") == 9) bits |= 32;
        Collections.sort(letters);
        if (letters.get(0) == 'a' && letters.get(3) == 'z') bits |= 64;
        return bits;
    }

    /** Strings: each check one bit. */
    public static int stringChecks() {
        int bits = 0;
        String[] parts = "a,b,,c,,".split(",");
        if (parts.length == 4 && parts[3].equals("c") && parts[2].isEmpty()) bits |= 1;
        if (String.join("-", List.of("x", "y", "z")).equals("x-y-z")) bits |= 2;
        if (String.format("%5.2f|%-4s|%04d|%x|%,d", 3.14159, "ab", 42, -1, 1234567)
                .equals(" 3.14|ab  |0042|ffffffff|1,234,567")) bits |= 4;
        if ("MiXeD".toLowerCase().equals("mixed") && "MiXeD".toUpperCase().equals("MIXED")) bits |= 8;
        char[] cs = "hey".toCharArray();
        if (cs.length == 3 && cs[1] == 'e' && String.valueOf(cs).equals("hey")) bits |= 16;
        if ("a.b.c".lastIndexOf('.') == 3 && "abcabc".indexOf("c", 3) == 5) bits |= 32;
        if ("ab".repeat(3).equals("ababab") && "  x ".strip().equals("x") && "   ".isBlank()) bits |= 64;
        if ("%s is %d".formatted("x", 5).equals("x is 5")) bits |= 128;
        return bits;
    }

    /** The functional interfaces' methods: each check one bit. */
    public static int functionChecks() {
        int bits = 0;
        List<String> words = new ArrayList<>(List.of("pear", "fig", "apple", "kiwi", "date"));
        words.sort(Comparator.comparingInt(String::length).thenComparing(Function.identity()));
        if (String.join(",", words).equals("fig,date,kiwi,pear,apple")) bits |= 1;
        Comparator<String> byLength = (a, b) -> a.length() - b.length();
        words.sort(byLength.reversed().thenComparing(Comparator.reverseOrder()));
        if (words.get(0).equals("apple") && words.get(1).equals("pear") && words.get(4).equals("fig")) bits |= 2;
        List<String> withNull = new ArrayList<>(java.util.Arrays.asList("b", null, "a"));
        withNull.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
        if (withNull.get(0) == null && withNull.get(1).equals("a")) bits |= 4;
        List<Double> ds = new ArrayList<>(List.of(2.5, -0.0, 0.0, -1.0));
        ds.sort(Comparator.comparingDouble(d -> d));
        if (ds.get(0) == -1.0 && 1 / ds.get(1) < 0 && 1 / ds.get(2) > 0) bits |= 8;
        Predicate<Integer> even = x -> x % 2 == 0;
        Predicate<Integer> big = x -> x > 10;
        if (even.and(big).test(12) && !even.and(big).test(8) && even.negate().test(3)
                && Predicate.not(big).test(1) && even.or(big).test(11)) bits |= 16;
        Function<Integer, Integer> plus1 = x -> x + 1;
        Function<Integer, Integer> times2 = x -> x * 2;
        if (plus1.andThen(times2).apply(3) == 8 && plus1.compose(times2).apply(3) == 7
                && Function.<Integer>identity().apply(9) == 9) bits |= 32;
        if (Comparator.<Integer>reverseOrder().compare(1, 2) > 0) bits |= 64;
        return bits;
    }

    /** BitSet: each check one bit. */
    public static int bitsetChecks() {
        int bits = 0;
        BitSet b = new BitSet();
        b.set(3);
        b.set(70, 130);
        b.clear(100);
        if (b.get(3) && !b.get(4) && b.get(70) && b.get(129) && !b.get(130) && !b.get(100)) bits |= 1;
        if (b.nextSetBit(4) == 70 && b.nextClearBit(70) == 100 && b.previousSetBit(69) == 3) bits |= 2;
        if (b.length() == 130 && b.cardinality() == 1 + 60 - 1) bits |= 4;
        BitSet c = new BitSet(256);
        c.set(3);
        c.set(200);
        BitSet both = (BitSet) b.clone();
        both.and(c);
        if (both.cardinality() == 1 && both.get(3)) bits |= 8;
        BitSet any = new BitSet();
        any.or(c);
        if (any.equals(c) && any.hashCode() == c.hashCode() && c.size() == 256) bits |= 16;
        BitSet small = new BitSet();
        small.set(1);
        small.set(5);
        if (small.toString().equals("{1, 5}") && !small.isEmpty() && new BitSet().isEmpty()) bits |= 32;
        small.flip(0, 3);
        if (small.get(0) && !small.get(1) && small.get(2) && small.get(5)) bits |= 64;
        return bits;
    }

    /** A list of the numbers below n: only get and size of its own. */
    static final class Below extends java.util.AbstractList<Integer> {
        final int n;

        Below(int n) {
            this.n = n;
        }

        @Override
        public Integer get(int i) {
            return i;
        }

        @Override
        public int size() {
            return n;
        }
    }

    /** The JDK's skeleton collections under a program's own: each check one bit. */
    public static int abstractChecks() {
        int bits = 0;
        Below b = new Below(4);
        if (b.contains(3) && !b.contains(4) && b.indexOf(2) == 2) bits |= 1;
        if (b.toString().equals("[0, 1, 2, 3]")) bits |= 2;
        if (b.equals(List.of(0, 1, 2, 3)) && b.hashCode() == List.of(0, 1, 2, 3).hashCode()) bits |= 4;
        int sum = 0;
        for (int x : b) sum += x;
        if (sum == 6 && !b.isEmpty() && new Below(0).isEmpty()) bits |= 8;
        return bits;
    }
}
