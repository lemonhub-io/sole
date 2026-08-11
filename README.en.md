# Sole

> A statically-typed, indentation-based general-purpose language designed for
> collaborative programming between humans and AI.
> Design document: [docs/GOALS.md](docs/GOALS.md) (Chinese; English translation:
> [docs/GOALS.en.md](docs/GOALS.en.md))

## Status

**M1 complete (lexer + parser + tree-walking interpreter + simple type checking).**
**M2 complete (full static type checking + move semantics + borrow checking + List/struct/interface).**

Currently supported:

- Indentation-based syntax: `fn` / `let` / assignment / `if-else` / `while` /
  `for` / `return`
- Postfix type annotations and square-bracket generics (`List[int]`, etc.)
- Full static type checking (annotation consistency, assignment, function
  signatures, operators, loop iterables, list elements; error codes E03xx)
- Move semantics (use-after-move errors for non-Copy values) and borrow
  checking (use-after-move, moves while borrowed, mutable borrow conflicts,
  borrow escape; error codes E04xx)
- Collection type `List[T]` (literal `[1, 2]`, indexing, `len`/`push`/`get`/`set`)
- `struct` / `interface` / `impl` with method calls (including borrowed `self`)
- Builtin functions `print` and `range`
- Mutability checking (`let mut` is required for reassignment); errors are
  located with line/column in lexing, parsing, type checking, and evaluation
- Bilingual error messages (`--lang en|zh` or the `SOLE_LANG` env var)

## Quick Start (on any machine with Rust)

```sh
cargo run --bin sole -- run examples/hello.sole
cargo run --bin sole -- run examples/fib.sole
cargo run --bin sole -- run --lang zh examples/hello.sole   # Chinese error messages
cargo test --workspace
```

Error messages are English by default and carry stable error codes (e.g.
`[E0201]`); switch to Chinese with `--lang zh` or the `SOLE_LANG=zh` env var.

## Development Workflow

```sh
# Verify locally
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Provisional / Not Yet Implemented (M1/M2 notes)

- `and` / `or` / `not` are **provisional** boolean operator keywords
- `int` is currently `i64` (the design goal is arbitrary precision, GOALS §4.1)
- Integer division is truncating for now; truthiness is Python-style
  (provisional)
- Borrow checking is the M2 minimal rule set (variable-level and field-level
  borrows, borrow propagation); M3 completes fine-grained and index borrows
- Not implemented: full generics (user-defined generic types and
  constraints), remaining collections (`Dict`/`Set`/`Tuple`), concurrency
  primitives (`task_group` / `go` / `Chan`, keywords reserved)
- `ref` / `mut ref` have full borrow checking, but no runtime borrow-escape
  detection (guaranteed statically)
