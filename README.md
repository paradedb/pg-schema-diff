# pg-schema-diff

Compare PostgreSQL schema SQL files and inspect their parsed SQL representation.
Parsing uses `pg_query` 6.2, based on the PostgreSQL 17 parser.

## Build

Install Rust, a C compiler, libclang, and `protoc`. On Ubuntu:

```sh
sudo apt-get install clang libclang-dev protobuf-compiler
cargo build --locked
cargo test --locked
```

`pg_query` compiles bundled PostgreSQL parser sources. It does not need a running
database or the old parser's PostgreSQL source download and LLVM LTO build.

## Usage

```sh
pg-schema-diff diff old.sql new.sql
pg-schema-diff deparse schema.sql
```

`diff` emits removals before additions and replacements. The existing limited
ALTER/DROP support remains: this is not a general-purpose migration planner.
`deparse` prints BEFORE/AFTER blocks and checks that the output parses to the same
AST, ignoring source positions. If the upstream deparser loses quoting or changes
the tree, the tool preserves the original SQL instead. The files should contain
server-side SQL, without psql commands such as `\echo`.
