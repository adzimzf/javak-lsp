package java.util;

import java.util.stream.Stream;

public interface Collection<E> extends Iterable<E> {
    int size();
    boolean isEmpty();
    boolean contains(Object o);
    boolean add(E e);
    boolean remove(Object o);
    void clear();
    default Stream<E> stream() { return null; }
    Object[] toArray();
    <T> T[] toArray(T[] a);
}
