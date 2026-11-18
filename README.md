# The Check Programming Language

A rust hosted, beginner friendly programming language that checks your work. Compiles to WebAssembly.

## Build

```
cargo build
```

## Run

Run a program:

```
cargo run -q -p check-cli -- run <file>
```

Check a program:

```
cargo run -q -p check-cli -- check <file>
```

Run language tests:

```
cargo run -q -p check-cli -- test silver_tests/
```

## Install

Install `check` to `~/.cargo/bin`:

```
cargo install --path crates/check-cli
```

## Rust tests

```
cargo test
cargo clippy --all-targets
cargo fmt --check
```
