package java.lang;

public class Object {
    public Object() {}
    public final native Class<?> getClass();
    public native int hashCode();
    public boolean equals(Object obj) { return this == obj; }
    protected native Object clone() throws CloneNotSupportedException;
    public String toString() { return null; }
    public final native void notify();
    public final native void notifyAll();
    public final void wait() throws InterruptedException {}
    protected void finalize() throws Throwable {}
}
