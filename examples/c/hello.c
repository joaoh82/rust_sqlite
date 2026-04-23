/*
 * Minimal C sample against the SQLRite FFI surface.
 *
 * Walks through:
 *   - opening an in-memory connection
 *   - CREATE TABLE + INSERT via sqlrite_execute
 *   - SELECT via sqlrite_query + sqlrite_step + sqlrite_column_*
 *   - explicit memory freeing (sqlrite_free_string, sqlrite_finalize,
 *     sqlrite_close)
 *   - a BEGIN / ROLLBACK transaction block
 *
 * Build + run:
 *   cd sqlrite-ffi && cargo build --release
 *   cc -I../sqlrite-ffi/include \
 *      -L../target/release -lsqlrite_c \
 *      examples/c/hello.c -o hello
 *   # macOS: also pass -Wl,-rpath,../target/release
 *   ./hello
 *
 * (On Linux, add `-Wl,-rpath,$PWD/../target/release` so the runtime
 * can find libsqlrite_c.so without LD_LIBRARY_PATH juggling.)
 */

#include <stdio.h>
#include <stdlib.h>
#include "sqlrite.h"

/* Small helper that prints the library's last error and exits. */
static void die(const char *what) {
    const char *err = sqlrite_last_error();
    fprintf(stderr, "%s: %s\n", what, err ? err : "(no error message)");
    exit(1);
}

int main(void) {
    struct SqlriteConnection *conn = NULL;
    if (sqlrite_open_in_memory(&conn) != Ok) {
        die("sqlrite_open_in_memory");
    }

    if (sqlrite_execute(conn,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);") != Ok) {
        die("CREATE TABLE");
    }
    if (sqlrite_execute(conn,
            "INSERT INTO users (name, age) VALUES ('alice', 30);") != Ok) {
        die("INSERT alice");
    }
    if (sqlrite_execute(conn,
            "INSERT INTO users (name, age) VALUES ('bob', 25);") != Ok) {
        die("INSERT bob");
    }

    /* Show the table. */
    struct SqlriteStatement *stmt = NULL;
    if (sqlrite_query(conn, "SELECT id, name, age FROM users;", &stmt) != Ok) {
        die("sqlrite_query");
    }

    int col_count = 0;
    sqlrite_column_count(stmt, &col_count);
    printf("Columns: ");
    for (int i = 0; i < col_count; i++) {
        char *name = NULL;
        sqlrite_column_name(stmt, i, &name);
        printf("%s%s", name, i + 1 < col_count ? ", " : "\n");
        sqlrite_free_string(name);
    }

    while (1) {
        enum SqlriteStatus st = sqlrite_step(stmt);
        if (st == Done) break;
        if (st != Row) die("sqlrite_step");

        int64_t id = 0;
        char *name = NULL;
        int64_t age = 0;
        sqlrite_column_int64(stmt, 0, &id);
        sqlrite_column_text(stmt, 1, &name);
        sqlrite_column_int64(stmt, 2, &age);
        printf("  row: id=%lld name=%s age=%lld\n",
               (long long)id, name, (long long)age);
        sqlrite_free_string(name);
    }
    sqlrite_finalize(stmt);

    /* Transaction that gets rolled back: the row doesn't survive. */
    sqlrite_execute(conn, "BEGIN;");
    sqlrite_execute(conn, "INSERT INTO users (name, age) VALUES ('phantom', 99);");
    printf("\nIn transaction: %d\n", sqlrite_in_transaction(conn));
    sqlrite_execute(conn, "ROLLBACK;");
    printf("After rollback: %d\n", sqlrite_in_transaction(conn));

    /* Count rows post-rollback — should be 2. */
    sqlrite_query(conn, "SELECT id FROM users;", &stmt);
    int count = 0;
    while (sqlrite_step(stmt) == Row) count++;
    printf("Row count after rollback: %d (expected 2)\n", count);
    sqlrite_finalize(stmt);

    sqlrite_close(conn);
    return 0;
}
