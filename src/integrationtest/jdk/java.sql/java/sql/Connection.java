package java.sql;

public interface Connection extends AutoCloseable {
    void commit() throws SQLException;
    void close() throws SQLException;
}
