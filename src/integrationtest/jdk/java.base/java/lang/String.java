package java.lang;

import java.nio.charset.Charset;

public final class String implements CharSequence, Comparable<String> {
    public String() {}
    public String(String original) {}
    public String(char[] value) {}
    public String(char[] value, int offset, int count) {}
    public String(int[] codePoints, int offset, int count) {}
    @Deprecated
    public String(byte[] ascii, int hibyte, int offset, int count) {}
    @Deprecated
    public String(byte[] ascii, int hibyte) {}
    public String(byte[] bytes, int offset, int length, String charsetName) {}
    public String(byte[] bytes, int offset, int length, Charset charset) {}
    public String(byte[] bytes, String charsetName) {}
    public String(byte[] bytes, Charset charset) {}
    public String(byte[] bytes, int offset, int length) {}
    public String(byte[] bytes) {}
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
    public static String format(java.util.Locale l, String format, Object... args) { return null; }
    public static String valueOf(Object obj) { return null; }
}
