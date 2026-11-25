# The Check Programming Language

A rust hosted, beginner friendly programming language that checks your work. Compiles to WebAssembly.

## Build

The WASM runtime needs Rust's `wasm32-unknown-unknown` target:

```
rustup target add wasm32-unknown-unknown
cargo build
```

To build without running WASM (and without `wasmtime`), use `cargo build --no-default-features -p check-cli`.

## Run

Run a program:

```
cargo run -q -p check-cli -- run <file>
```

`run --wasm <file>` compiles it to WebAssembly and runs that instead. Only part of the language compiles so far.

Check a program:

```
cargo run -q -p check-cli -- <file>
```

Apply the fixes that have one obvious answer, showing each change and its rule (`--only style` or `--only error` limits the levels; `--dry-run` shows the changes without writing them, and fails if there are any):

```
cargo run -q -p check-cli -- tidy <file> [--only style,error] [--dry-run]
```

Run language tests:

```
cargo run -q -p check-cli -- test silver_tests/
```

`test --wasm` runs them through WebAssembly instead. A test whose folder has a `wasm_unsupported` file is skipped, since it uses something not compiled yet.

## Install

Install `check` to `~/.cargo/bin`:

```
cargo install --path crates/check-cli
```

To install `check` and the VS Code extension, which shows errors as you type and offers their fixes (needs Node and the `code` command):

```
scripts/install-editor.sh
```

The extension applies tidy's fixes on save. To turn that off, add to VS Code's settings:

```
"[check]": { "editor.codeActionsOnSave": { "source.fixAll.check": "never" } }
```

For a dev build, set `check.serverPath` to `target/debug/check`'s full path.

## Rust tests

```
cargo test
cargo clippy --all-targets
cargo fmt --check
```
