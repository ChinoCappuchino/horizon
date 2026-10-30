package t;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
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
}
