# Sole

> A statically-typed, indentation-based general-purpose language designed for
> collaborative programming between humans and AI.
> Design document: [docs/GOALS.md](docs/GOALS.md) (Chinese; English translation:
> [docs/GOALS.en.md](docs/GOALS.en.md))

## Status

**M1 complete (lexer + parser + tree-walking interpreter + simple type checking).**
**M2 complete (full static type checking + move semantics + borrow checking + List/struct/interface).**
**M3 complete (bytecode VM + coroutine scheduler + channel runtime).**
**M4 complete (stdlib core + formatter + LSP + real-project acceptance).**

Currently supported:

- Indentation-based syntax: `fn` / `let` / assignment / `if-else` / `while` /
  `for` / `return`, and the boolean operators `and` / `or` / `not`
- Postfix type annotations and square-bracket generics (`List[int]`, etc.)
- Full static type checking (annotation consistency, assignment, function
  signatures, operators, loop iterables, list elements; error codes E03xx)
- Move semantics (use-after-move errors for non-Copy values) and borrow
  checking (use-after-move, moves while borrowed, mutable borrow conflicts,
  borrow escape; error codes E04xx)
- Collection type `List[T]` (literal `[1, 2]`, indexing,
  `len`/`push`/`get`/`set`/`contains`)
- `struct` / `interface` / `impl` with method calls (including borrowed `self`)
- Explicit error handling: `Option[T]` (`Some`/`None`,
  `is_some`/`is_none`/`unwrap`) and `Result[T, E]` (`Ok`/`Err`,
  `is_ok`/`is_err`/`unwrap`/`unwrap_err`)
- Generic functions: `fn max[T: Comparable](a: T, b: T) -> T`, instantiated at
  call sites (bound error E0320)
- More collections: `Dict[K, V]` (indexing,
  `len`/`get`/`set`/`contains`/`remove`/`keys`/`values`), `Set[T]`
  (`len`/`add`/`contains`/`remove`, unique elements), `Tuple[...]` (indexing, `len`)
- `str` methods: `len`/`sub`/`split`/`join`/`contains`/`starts_with`/
  `ends_with`/`trim`/`to_str`/`to_int`/`to_float` (`to_int`/`to_float` return `Result`)
- Testing primitives: `test` blocks + `assert` statements (E0221), run with
  `sole test <file>`
- Module mechanism: `import foo` / `from foo import a, b` (multi-file loading,
  cycle dedup, prefix rewriting)
- Stdlib builtins (globally available):
  - IO: `read_to_str(path)` / `write(path, s)` (return `Result`)
  - Time: `clock()` (milliseconds) / `sleep(ms)`
  - Math: `abs`/`floor`/`ceil`/`round`/`sqrt`/`pow`
  - JSON: `json_encode(v)` / `json_decode(s)` plus the dynamic `Json` type
    (indexable, comparable with `None`,
    `len`/`contains`/`keys`/`is_int`/`is_str`/`to_str`)
- Bytecode VM: AST → bytecode (compiler + stack VM), locals value semantics
  with lazy borrow cells, `&Instr` reference dispatch, run-until-block
  cooperative scheduling (performance on par with CPython, see `bench/`)
- Concurrency primitives: coroutines (`go`), `task_group` (structured
  concurrency: scoped waiting + cancellation propagation), channels `Chan[T]`
  (`send`/`recv`/`close`, buffered/unbuffered, `for v in ch` receive loops, `yield`)
- Formatter: the official canonical format (gofmt-like),
  `sole fmt <file|dir>` / `sole fmt --check`
- LSP server: `sole lsp` (full-sync diagnostics, go-to-definition,
  completion; reuses the type checker with bilingual rendering)
- Mutability checking (`let mut` is required for reassignment); errors are
  located with line/column in lexing, parsing, type checking, and evaluation
- Bilingual error messages (`--lang en|zh` or the `SOLE_LANG` env var)

## Quick Start (on any machine with Rust)

```sh
cargo run --bin sole -- run examples/hello.sole
cargo run --bin sole -- run examples/fib.sole
cargo run --bin sole -- run examples/list.sole       # List + borrows (M2)
cargo run --bin sole -- run examples/borrow.sole     # ref / mut ref (M2)
cargo run --bin sole -- run examples/shapes.sole     # struct / interface (M2)
cargo run --bin sole -- run examples/option.sole     # Option / Result (M4)
cargo run --bin sole -- run examples/collections.sole # Dict / Set / Tuple (M4)
cargo run --bin sole -- run examples/generics.sole   # generic functions (M4)
cargo run --bin sole -- run examples/str_methods.sole # str methods (M4)
cargo run --bin sole -- run examples/json_usage.sole  # stdlib JSON (M4)
cargo run --bin sole -- run examples/concurrency.sole # go / task_group / Chan (M3)
cargo run --bin sole -- test examples/phase0.sole    # run `test` blocks (M4)
cargo run --bin sole -- fmt --check examples         # formatter check (M4)
cargo run --bin sole -- run --lang zh examples/hello.sole   # Chinese error messages
cargo run --bin sole -- run bench/fib_sum.sole       # performance benchmark (M3, vs CPython)
cargo test --workspace
```

Real small project (M4 acceptance): `examples/projects/json_tool` — a JSON
command-line tool (multi-file modules + stdlib + 41 `test` cases):

```sh
cargo run --bin sole -- test examples/projects/json_tool/main.sole
```

Note: the CLI does not yet pass arguments to scripts — the demo drives
`main(args)` from `sole test` blocks (deferred to M5).

Error messages are English by default and carry stable error codes (e.g.
`[E0201]`); switch to Chinese with `--lang zh` or the `SOLE_LANG=zh` env var.

## Development Workflow

```sh
# Verify locally
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Provisional / Not Yet Implemented

- `int` is currently `i64` (the design goal is arbitrary precision, GOALS §4.1)
- Integer division is truncating for now; truthiness is Python-style
  (provisional)
- Borrow checking is the minimal rule set (variable-level and field-level
  borrows, borrow propagation); fine-grained regions and index borrows are
  not implemented
- Not implemented: `select` multiplexing, `Shared[T]` / `Mutex` (GOALS §7.5),
  user-defined generic types/interfaces (explicitly excluded by D9)
- `ref` / `mut ref` have full borrow checking, but no runtime borrow-escape
  detection (guaranteed statically)
- The formatter discards comments (the lexer does not keep them); LSP hover
  and incremental sync are not implemented (honestly recorded in D14)
