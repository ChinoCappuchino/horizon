package t;

import java.io.InputStream;
import java.math.BigInteger;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;

public final class BigIntegerProbe {
    /** Custom Number subclass in the program, exercising Number.<init> and inherited byteValue/shortValue. */
    static final class HalfNumber extends Number {
        private final long raw;

        HalfNumber(long raw) {
            this.raw = raw;
        }

        @Override
        public int intValue() {
            return (int) (raw / 2);
        }

        @Override
        public long longValue() {
            return raw / 2;
        }

        @Override
        public float floatValue() {
            return (float) (raw / 2.0);
        }

        @Override
        public double doubleValue() {
            return raw / 2.0;
        }
    }

    static long throughNumber(Number n) {
        return n.longValue()
            ^ ((long) n.intValue() << 1)
            ^ ((long) n.shortValue() << 2)
            ^ ((long) n.byteValue() << 3)
            ^ (long) n.doubleValue()
            ^ (long) n.floatValue();
    }

    static boolean checkMathExact() {
        // Normal, zero, and negative values across all 5 overloads.
        if (Math.addExact(0, 0) != 0
                || Math.addExact(15, -27) != -12
                || Math.addExact(-100, -200) != -300
                || Math.addExact(0L, 0L) != 0L
                || Math.addExact(150_000_000_000L, -270_000_000_000L) != -120_000_000_000L
                || Math.multiplyExact(0, -999) != 0
                || Math.multiplyExact(-12, 13) != -156
                || Math.multiplyExact(-12, -13) != 156
                || Math.multiplyExact(0L, -999) != 0L
                || Math.multiplyExact(-100_000_000L, 300) != -30_000_000_000L
                || Math.multiplyExact(0L, -999L) != 0L
                || Math.multiplyExact(-100_000_000L, -300L) != 30_000_000_000L) {
            return false;
        }

        // Boundary values that do NOT overflow.
        if (Math.addExact(Integer.MAX_VALUE, 0) != Integer.MAX_VALUE
                || Math.addExact(Integer.MIN_VALUE, 0) != Integer.MIN_VALUE
                || Math.addExact(Integer.MAX_VALUE, -1) != Integer.MAX_VALUE - 1
                || Math.addExact(Integer.MIN_VALUE, 1) != Integer.MIN_VALUE + 1
                || Math.addExact(Integer.MAX_VALUE, Integer.MIN_VALUE) != -1
                || Math.addExact(Long.MAX_VALUE, 0L) != Long.MAX_VALUE
                || Math.addExact(Long.MIN_VALUE, 0L) != Long.MIN_VALUE
                || Math.addExact(Long.MAX_VALUE, -1L) != Long.MAX_VALUE - 1L
                || Math.addExact(Long.MIN_VALUE, 1L) != Long.MIN_VALUE + 1L
                || Math.addExact(Long.MAX_VALUE, Long.MIN_VALUE) != -1L
                || Math.multiplyExact(Integer.MAX_VALUE, 1) != Integer.MAX_VALUE
                || Math.multiplyExact(Integer.MAX_VALUE, -1) != -Integer.MAX_VALUE
                || Math.multiplyExact(Integer.MIN_VALUE, 1) != Integer.MIN_VALUE
                || Math.multiplyExact(Integer.MIN_VALUE, 0) != 0
                || Math.multiplyExact(Long.MAX_VALUE, 1) != Long.MAX_VALUE
                || Math.multiplyExact(Long.MAX_VALUE, -1) != -Long.MAX_VALUE
                || Math.multiplyExact(Long.MIN_VALUE, 1) != Long.MIN_VALUE
                || Math.multiplyExact(Long.MIN_VALUE, 0) != 0L
                || Math.multiplyExact(Long.MAX_VALUE, 1L) != Long.MAX_VALUE
                || Math.multiplyExact(Long.MAX_VALUE, -1L) != -Long.MAX_VALUE
                || Math.multiplyExact(Long.MIN_VALUE, 1L) != Long.MIN_VALUE
                || Math.multiplyExact(Long.MIN_VALUE, 0L) != 0L) {
            return false;
        }

        // Overflows: must throw ArithmeticException and be caught by Java try/catch.
        int caught = 0;
        try {
            caught += Math.addExact(Integer.MAX_VALUE, 1);
            return false;
        } catch (ArithmeticException e) {
            if ("integer overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += Math.addExact(Integer.MIN_VALUE, -1);
            return false;
        } catch (ArithmeticException e) {
            if ("integer overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.addExact(Long.MAX_VALUE, 1L);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.addExact(Long.MIN_VALUE, -1L);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += Math.multiplyExact(Integer.MAX_VALUE, 2);
            return false;
        } catch (ArithmeticException e) {
            if ("integer overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += Math.multiplyExact(Integer.MIN_VALUE, -1);
            return false;
        } catch (ArithmeticException e) {
            if ("integer overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.multiplyExact(Long.MAX_VALUE, 2);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.multiplyExact(Long.MIN_VALUE, -1);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.multiplyExact(Long.MAX_VALUE, 2L);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            caught += (int) Math.multiplyExact(Long.MIN_VALUE, -1L);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        try {
            Duration.ofDays(Long.MAX_VALUE);
            return false;
        } catch (ArithmeticException e) {
            if ("long overflow".equals(e.getMessage())) caught++;
        }
        return caught == 11;
    }

    /** Checks 0, 1, -1, 42, Long.MIN_VALUE, Long.MAX_VALUE, Number dispatch, repeated ops, Duration, and Math.*Exact. */
    public static int checkAll() {
        int bits = 0;

        // Bit 0: 0, 1, -1, 42.
        if (BigInteger.valueOf(0L).longValue() == 0L
                && BigInteger.valueOf(1L).longValue() == 1L
                && BigInteger.valueOf(-1L).longValue() == -1L
                && BigInteger.valueOf(42L).longValue() == 42L
                && BigInteger.valueOf(0L) == BigInteger.ZERO
                && BigInteger.valueOf(1L) == BigInteger.ONE) {
            bits |= 1;
        }

        // Bit 1: Long.MIN_VALUE and Long.MAX_VALUE.
        BigInteger min = BigInteger.valueOf(Long.MIN_VALUE);
        BigInteger max = BigInteger.valueOf(Long.MAX_VALUE);
        if (min.longValue() == Long.MIN_VALUE
                && max.longValue() == Long.MAX_VALUE
                && min.intValue() == (int) Long.MIN_VALUE
                && max.intValue() == (int) Long.MAX_VALUE
                && min.shortValue() == (short) Long.MIN_VALUE
                && max.shortValue() == (short) Long.MAX_VALUE
                && min.byteValue() == (byte) Long.MIN_VALUE
                && max.byteValue() == (byte) Long.MAX_VALUE
                && min.negate().compareTo(max) > 0
                && max.add(BigInteger.ONE).negate().equals(min)) {
            bits |= 2;
        }

        // Bit 2: Calls through a Number variable (BigInteger, runtime boxes, custom subclass).
        long[] sample = {0L, 1L, -1L, 42L, 300L, -300L, Long.MIN_VALUE, Long.MAX_VALUE};
        boolean numOk = true;
        for (long v : sample) {
            Number bi = BigInteger.valueOf(v);
            if (bi.longValue() != v
                    || bi.intValue() != (int) v
                    || bi.shortValue() != (short) v
                    || bi.byteValue() != (byte) v
                    || bi.doubleValue() != (double) v
                    || bi.floatValue() != (float) v) {
                numOk = false;
            }
            if (throughNumber(bi) != throughNumber(Long.valueOf(v))) {
                numOk = false;
            }
        }
        Number half = new HalfNumber(600L);
        if (half.longValue() != 300L
                || half.intValue() != 300
                || half.shortValue() != (short) 300
                || half.byteValue() != (byte) 300) {
            numOk = false;
        }
        if (numOk) {
            bits |= 4;
        }

        // Bit 3: Repeated operations (cache hit/miss, arithmetic, division beyond 64 bits).
        BigInteger acc = BigInteger.ZERO;
        for (int i = -64; i <= 64; i++) {
            BigInteger step = BigInteger.valueOf(i);
            acc = acc.add(step.multiply(BigInteger.valueOf(7L)));
        }
        BigInteger wide = max.multiply(BigInteger.valueOf(3L)).add(BigInteger.valueOf(19L));
        BigInteger[] qr = wide.divideAndRemainder(max);
        if (acc.longValue() == 0L
                && BigInteger.valueOf(16L) == BigInteger.valueOf(16L)
                && BigInteger.valueOf(-16L) == BigInteger.valueOf(-16L)
                && qr[0].longValue() == 3L
                && qr[1].longValue() == 19L) {
            bits |= 8;
        }

        // Bit 4: java.time.Duration (including ofSeconds(JJ), plusMillis, ofDays, ofHours, ofMinutes, toMillis, toNanos)
        // and primitive TYPE mirrors used by BigInteger.<clinit>.
        Duration d = Duration.ofSeconds(3L, 500_000_000L).plusMillis(250L);
        Duration combo = Duration.ofDays(1L).plusHours(2L).plusMinutes(3L);
        if (d.toMillis() == 3750L
                && combo.toNanos() == 93_780_000_000_000L
                && Duration.ZERO.toNanos() == 0L
                && Integer.TYPE == int.class
                && Integer.TYPE.isPrimitive()
                && Long.TYPE == long.class) {
            bits |= 16;
        }

        // Bit 5: Math.addExact and Math.multiplyExact (normal, zero, negative, limits, and caught ArithmeticException).
        if (checkMathExact()) {
            bits |= 32;
        }

        return bits;
    }

    static void dumpJdk(Path dir) throws Exception {
        String[] names = {
            "java/lang/Object",
            "java/lang/Number",
            "java/math/BigInteger",
            "java/math/MutableBigInteger",
            "java/math/SignedMutableBigInteger",
            "java/math/BigInteger$RecursiveOp",
            "java/io/ObjectStreamField",
            "java/time/Duration",
            "java/util/concurrent/ForkJoinTask",
            "java/util/concurrent/RecursiveTask",
            "java/util/AbstractMap",
            "java/util/HashMap",
            "java/util/LinkedHashMap",
        };
        for (String n : names) {
            Path out = dir.resolve(n + ".class");
            Files.createDirectories(out.getParent());
            try (InputStream in = Object.class.getResourceAsStream("/" + n + ".class")) {
                if (in != null) {
                    Files.write(out, in.readAllBytes());
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        if (args.length == 2 && "--dump-jdk".equals(args[0])) {
            dumpJdk(Path.of(args[1]));
        }
        int bits = checkAll();
        if (bits != 0b111111) {
            throw new AssertionError("BigIntegerProbe failed: bits=" + Integer.toBinaryString(bits));
        }
        System.out.println(BigInteger.valueOf(42L).longValue());
    }
}
