package java.util.stream;

import java.util.List;
import java.util.function.Function;

public interface Stream<T> {
    <R> Stream<R> map(Function<? super T, ? extends R> mapper);
    <R, A> R collect(Collector<? super T, A, R> collector);
    List<T> toList();
    Object[] toArray();
    long count();
    public static <T> Stream<T> of(T t) { return null; }
    public static <T> Stream<T> of(T... values) { return null; }
}
