package java.lang;

import java.io.PrintStream;

public final class System {
    private System() {}
    public static final PrintStream out = null;
    public static final PrintStream err = null;
    public static long currentTimeMillis() { return 0; }
    public static void exit(int status) {}
}
