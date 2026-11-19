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

## Install

Install `check` to `~/.cargo/bin`:

```
cargo install --path crates/check-cli
```

To install `check` and the VS Code extension, which shows errors as you type and offers their fixes (needs Node and the `code` command):

```
scripts/install-editor.sh
```

To apply tidy's fixes on save, add to VS Code's settings:

```
"editor.codeActionsOnSave": { "source.fixAll.check": "explicit" }
```

For a dev build, set `check.serverPath` to `target/debug/check`'s full path.

## Rust tests

```
cargo test
cargo clippy --all-targets
cargo fmt --check
```
