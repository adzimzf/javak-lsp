package java.util.stream;

import java.util.List;
import java.util.function.Function;
import java.util.function.IntFunction;
import java.util.function.Predicate;

public interface Stream<T> {
    Stream<T> filter(Predicate<? super T> predicate);
    <R> Stream<R> map(Function<? super T, ? extends R> mapper);
    <R, A> R collect(Collector<? super T, A, R> collector);
    List<T> toList();
    Object[] toArray();
    <A> A[] toArray(IntFunction<A[]> generator);
    long count();
    public static <T> Stream<T> of(T t) { return null; }
    public static <T> Stream<T> of(T... values) { return null; }
}
