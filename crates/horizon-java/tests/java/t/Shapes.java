package t;

import g.Block;

// Test input for the compiler (tests/extend.rs): classes that extend a game
// class, virtual calls, interfaces, records, enums, arrays, instanceof and
// string concatenation.
public class Shapes {
    interface Shape {
        double area();

        default String kind() { return "shape"; }
    }

    record Square(double side) implements Shape {
        public double area() { return side * side; }

        public String kind() { return "square"; }
    }

    abstract static class Base implements Shape {
        abstract int sides();

        public double area() { return sides(); }
    }

    static class Tri extends Base {
        int sides() { return 3; }
    }

    static class Quad extends Base {
        int sides() { return 4; }

        public double area() { return 40; }
    }

    enum Color { RED, GREEN, BLUE }

    static class Lamp extends Block {
        int uses;

        Lamp() { super(7); }

        @Override
        public int use(int x) {
            uses++;
            return x + light + super.use(100);
        }
    }

    public static double total() {
        Shape[] shapes = {new Square(2), new Tri(), new Quad()};
        double t = 0;
        for (Shape s : shapes) t += s.area();
        return t;
    }

    public static String kinds() { return new Square(1).kind() + "," + new Tri().kind(); }

    public static int colors() { return Color.values().length * 10 + Color.BLUE.ordinal(); }

    public static boolean isShape(Object o) { return o instanceof Shape; }

    public static Object square() { return new Square(1); }

    public static int arrays() {
        int[] a = new int[5];
        for (int i = 0; i < a.length; i++) a[i] = i * i;
        byte[] b = {(byte) 200, 1};
        char[] c = {'a', 'b'};
        long[] l = {1L << 40};
        int[] copy = a.clone();
        copy[0] = 99;
        return a[4] + b[0] + c[1] + (int) (l[0] >> 40) + a[0] + copy[0];
    }

    public static int lamp() { return new Lamp().use(1); }

    public static Block makeLamp() { return new Lamp(); }

    public static Object facing() { return Lamp.FACING; }

    public static String squareText() { return new Square(3).toString(); }

    public static boolean squaresEqual() { return new Square(3).equals(new Square(3)); }

    public static String text(int i, String s, double d) { return "i=" + i + " s=" + s + " d=" + d; }

    public static Object tri() { return new Tri(); }

    public static Object bigSquare() { return new Square(9); }

    public static int pattern(Object o) {
        return switch (o) {
            case null -> -1;
            case Square s when s.side() > 5 -> 50;
            case Square s -> 5;
            case Tri t -> 3;
            case Shape s -> 1;
            default -> 0;
        };
    }
}
