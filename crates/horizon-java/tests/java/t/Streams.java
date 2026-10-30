package t;

import java.util.ArrayList;
import java.util.Arrays;
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
}
