package g;

// A stand-in for a game class (tests/extend.rs): compiled only so the test
// classes can extend it; the test's host plays it.
public class Block {
    public static final Object FACING = new Object();
    protected int light;

    public Block(int light) { this.light = light; }

    public int use(int x) { return x; }

    public int describe() { return use(1) * 10; }
}
