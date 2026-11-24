package java.util;

public interface List<E> extends Collection<E> {
    E get(int index);
    E set(int index, E element);
    void add(int index, E element);
    E remove(int index);
    int indexOf(Object o);
    List<E> subList(int fromIndex, int toIndex);
    static <E> List<E> of() { return null; }
    static <E> List<E> of(E... elements) { return null; }
}
