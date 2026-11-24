package java.lang;

public final class String implements CharSequence, Comparable<String> {
    public String() {}
    public String(String original) {}
    public String(char[] value) {}
    public int length() { return 0; }
    public boolean isEmpty() { return true; }
    public char charAt(int index) { return 0; }
    public boolean equals(Object anObject) { return false; }
    public int compareTo(String anotherString) { return 0; }
    public boolean startsWith(String prefix) { return false; }
    public int indexOf(String str) { return 0; }
    public String substring(int beginIndex) { return null; }
    public String concat(String str) { return null; }
    public String replace(CharSequence target, CharSequence replacement) { return null; }
    public boolean contains(CharSequence s) { return false; }
    public String trim() { return null; }
    public String toUpperCase() { return null; }
    public static String format(String format, Object... args) { return null; }
    public static String valueOf(Object obj) { return null; }
}
