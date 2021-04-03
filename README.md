Rust-SQLite (SQLRite)
===
[![Build Status](https://github.com/joaoh82/rust_sqlite/workflows/Rust/badge.svg)](https://github.com/joaoh82/rust_sqlite/actions)
[![dependency status](https://deps.rs/repo/github/joaoh82/rust_sqlite/status.svg)](https://deps.rs/repo/github/joaoh82/rust_sqlite)
[![Coverage Status](https://coveralls.io/repos/github/joaoh82/rust_sqlite/badge.svg?branch=main)](https://coveralls.io/github/joaoh82/rust_sqlite?branch=main)
[![Maintenance](https://img.shields.io/badge/maintenance-actively%20maintained-brightgreen.svg)](https://deps.rs/repo/github/joaoh82/rust_sqlite)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

`Rust-SQLite`, aka `SQLRite` , is a simple embedded database modeled off `SQLite`, but developed with `Rust`. The goal is get a better understanding of database internals by building one.

> What I cannot create, I do not understand. 
> — Richard Feynman

### Read the series of posts about it:
##### What would SQLite look like if written in Rust?
* [Part 0 - Overview](https://medium.com/the-polyglot-programmer/what-would-sqlite-would-look-like-if-written-in-rust-part-0-4fc192368984)
* [Part 1 - Understanding SQLite and Setting up CLI Application and REPL](https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-1-4a84196c217d)
* [Part 2 - SQL Statement and Meta Commands Parser + Error Handling](https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-2-55b30824de0c)

![The SQLite Architecture](images/architecture.png "The SQLite Architecture")

### Requirements
Before you begin, ensure you have met the following requirements:
* Rust (latest stable) – (How to install Rust)[https://www.rust-lang.org/en-US/install.html]

### Usage (TBD)

```shell
> ./rust_sqlite -- help
SQLRite 0.1.0
Joao Henrique Machado Silva <joaoh82@gmail.com>
Light version of SQLite developed with Rust

USAGE:
    rust_sqlite

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information
```

### Project Progress
*Not checked means I am currently working on.*
- [x] CLI and REPL Interface
- [x] Parse meta commands and sql commands.
- [x] Execute simple commands
- [x] Standarized error handling
- [x] Generic validation structure for SQL Commands.
- [x] `Create Table` Command Parsing
- [x] Improve error handling with https://github.com/dtolnay/thiserror
- [x] Added support for parsing duplicate columns on CREATE TABLE
- [x] Added support for parsing multiple PRIMARY KEY on CREATE TABLE
- [x] In memory BTreeMap indexes initially only for PRIMARY KEYS.
- [x] Simple INSERT queries command parsing.
- [x] Implementation UNIQUE key constraints.
- [ ] Simple SELECT queries (Single WHERE clause and no JOINS).
- [ ] Serialization | Deserialization to and from binary encodings ([bincode](https://crates.io/crates/bincode)).


### Roadmap
Features that are in the roadmap of the project:

*Ideally in order of priority, but nothing set in stone.*


- [ ] Implement Open command to load database with a command `.open`
- [ ] Joins
  - [ ] INNER JOIN (or sometimes called simple join)
  - [ ] LEFT OUTER JOIN (or sometimes called LEFT JOIN)
  - [ ] CROSS JOIN
  - The RIGHT OUTER JOIN and FULL OUTER JOIN are not supported in SQLite.
- [ ] WAL - Write Ahead Log Implementation
- [ ] `Pager Module` 
  - [ ] Implementing transactional ACID properties
  - [ ] Concurrency
  - [ ] Lock Manager
- [ ] Composite Indexing - cost and performance gain analysis
- [ ] Benchmarking vs SQLite for comparison
- [ ] Server Client / Connection Manager
- [ ] Different implementations of storage engines and data structures to optimize for different scenarios
  - [ ] Write Heavy - `LSM Tree && SSTable`
  - [ ] Read Heavy - `B-Tree`

### Contributing
**Pull requests are warmly welcome!!!**

For major changes, please [open an issue](https://github.com/joaoh82/rust_sqlite/issues/new) first and let's talk about it. We are all ears!

If you'd like to contribute, please fork the repository and make changes as you'd like and shoot a Pull Request our way!

**Please make sure to update tests as appropriate.**

If you feel like you need it go check the GitHub documentation on [creating a pull request](https://help.github.com/en/github/collaborating-with-issues-and-pull-requests/creating-a-pull-request).

### Code of Conduct

Contribution to the project is organized under the terms of the
Contributor Covenant, the maintainer of SQLRite, [@joaoh82](https://github.com/joaoh82), promises to
intervene to uphold that code of conduct.

### Contact

If you want to contact me you can reach me at <joaoh82@gmail.com>.

##### Inspiration
* https://cstack.github.io/db_tutorial/