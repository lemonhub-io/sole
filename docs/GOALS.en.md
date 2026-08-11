# Goals Document (GOALS)

> Language name: **Sole** · Version: v0.1 (draft) · Status: design phase
>
> This document is the language's "constitution". Every subsequent syntax
> reference, standard library design, and implementation decision must stay
> consistent with this document; in case of conflict, either change the
> decision or change this document (recording the reason).

---

## 1. Vision & Positioning

**One-line positioning:**

> A statically-typed, indentation-based general-purpose language designed for
> "collaborative programming between humans and AI" — taking the best of
> Python's and TypeScript's syntax, and eliminating all implicit magic in its
> semantics so that program behavior is fully predictable.

### 1.1 Target Users

| User | Need |
|------|------|
| Human developers | Fast code reading, low cognitive load, complete toolchain |
| AI coding agents (Copilot / Claude Code / Codex class) | High first-pass compile rate, locatable errors, canonical formatting, no ambiguity |
| Both collaborating | When humans review AI code, they can quickly grasp intent and boundaries |

### 1.2 Core Scenario

**Provisional: General Purpose** — no domain is bound up front; the
positioning will narrow if concrete needs arise later (2026-08-11)

**What the general-purpose positioning means right now:**
- Neither excludes nor promises any domain; the standard library keeps a
  balanced core, with priority driven by real needs (see §8)
- Several **soft preferences** are retained (from the earlier scripting
  discussion; no hard targets):
  - Fast startup and a build-step-free feedback loop (parse → type/borrow
    check → bytecode all at once before execution)
  - Shebang support (`#!/usr/bin/env sole`), scripts executable directly, no
    `main()` ceremony
  - Directly readable, actionable error messages (§9.2 — a hard requirement
    for any scenario)
- The "correctness-first" character of ownership and static types is the
  language's identity and does not shrink with the scenario

### 1.3 Anti-Goals

The following are **explicitly out of scope**, to prevent scope creep:

- ❌ No high-performance systems programming (not competing with C/Rust on raw
  performance; ownership is chosen for "GC-free determinism", not a
  performance race)
- ❌ No runtime metaprogramming / macro systems / code-gen reflection
  (conflicts with "explicit over implicit")
- ❌ No pursuit of syntactic sugar richness (less sugar = fewer ways for AI to
  get it wrong)
- ❌ Not source-compatible with Python/TS (not pretending to be their dialect)
- ❌ No dynamically-typed language (types are the first line of AI
  self-checking)

---

## 2. Core Design Principles

Every principle must be **executable and verifiable**, not a slogan.

### P1. One Obvious Way
Only one mainstream way to express any given thing. The language itself (not
community convention) rules out equivalent formulations as far as possible:
- No two loop keywords (`for` has exactly one form; no C-style
  `for/while/do-while` trio)
- No two ways to write equality (no `is`/`eq` on top of `==`)
- Before adding a feature, ask: "is there already an equivalent way to write
  this?" If yes, don't add it

### P2. Predictability
- Unambiguous syntax, no context-sensitivity (parsing does not depend on "what
  the previous line was")
- Deterministic evaluation order (no implementation-defined behavior)
- The same code always means the same thing, independent of imports or global
  state

### P3. Explicit Over Implicit
- **Magic methods are banned** (see §5): no `__add__`, `__eq__`, `__iter__`
  hidden hooks
- No implicit type conversions (even numeric conversions must be explicit)
- No implicit global imports, no implicit singletons, no implicit scope
  penetration

### P4. Readability First
Code is read/reviewed far more often than it is written — especially AI-
generated code, which humans will review repeatedly.

### P5. Static Types, Strong Inference
- Local variables are inferred where possible; public signatures must be
  explicitly annotated (public APIs are contracts, and contracts must be
  visible)
- Type errors must be reported at compile time with **locatable, fixable**
  error messages

### P6. AI-Generation Friendly
Quantifiable goals:
- ≥ 90% first-pass compile rate for AI-generated code on common tasks
- Compile errors contain: exact location + cause + fix suggestion + example
- The language ships a canonical formatter; AI never has to guess style

### P7. Small Core, Solid Stdlib
The language core is small and stable; capability comes mainly from a
small-but-refined standard library rather than syntax.

---

## 3. Syntax Decisions (Decided)

> This section records settled syntax decisions and their rationale.
> **Decided = not changed again unless a fatal flaw is found.**

### D1. Indentation-Based Blocks
- Python-like: indentation defines blocks; no `{}`, no `end` keyword
- Rationale: visual structure is direct, fewer tokens, AI aligns nested
  structures more reliably
- Hard rules:
  - Mixing tabs/spaces = **compile error** (not a warning)
  - Multi-line expressions only inside parentheses (same rule as Python); no
    implicit line continuation
  - No semicolons at end of line; one statement per line (`;` may remain as an
    explicit same-line separator, but the formatter expands it)

### D2. Postfix Static Types
- TypeScript-like: `x: int`, `fn add(a: int, b: int) -> int`
- Signatures read naturally left-to-right: parameter name first, type second,
  reading like an English sentence
- Rules:
  - **Function/method public signatures must be explicitly annotated** (both
    AI and humans need visible contracts)
  - Local variables can be inferred; annotations are optional: `let x = 5`
  - Inference is alias-friendly (inferred types are fully equivalent to
    hand-written annotations; no gradual typing)

### D3. No Magic Methods
- Python-style `__xxx__` hidden hooks are **completely banned**
- The language provides no "automatically takes effect once defined"
  mechanism
- The replacement mechanism is in §5 — this is the biggest fork between Sole
  and Python

### D4. Square-Bracket Generics
- Python-style: `List[int]`, `Dict[str, int]`, `Result[Ok, Err]`
- Rationale: avoids the ambiguity of `<>` with comparison/shift operators
  (the famous TS/JSX pain point); AI won't write the direction wrong
- Parsing rules (guaranteed unambiguous):
  - Generic arguments **only appear in type positions** (annotations, type
    aliases, generic constraints)
  - `[]` subscripting only appears in expression positions
  - The two positions are naturally distinguished by context; no ambiguity
    fallback needed

### D5. Naming Conventions (TBD, suggested as follows)

| Category | Suggestion | Rationale |
|----------|------------|-----------|
| Functions/variables/fields | `snake_case` | Consistent with the Python ecosystem, richest AI corpus |
| Types/interfaces/enums | `PascalCase` | Consistent with TS; type positions are recognizable at a glance |
| Constants | `SCREAMING_SNAKE` | Convention |
| Methods | `snake_case` | Uniformity |

> Note: a single naming rule is itself AI-friendly — the model never has to
> guess the style of a name.

### D6. Loop Forms: `for` + Explicit Iteration + Ownership Markers

**The only collection loop**: `for x in <iterable>`, whose semantics are
decided by "iterable + ownership marker" (decided 2026-08-11)

| Form | Semantics | Loop variable type | Translates to |
|------|-----------|--------------------|---------------|
| `for x in v` | **Move iteration**: consumes ownership of v; v unusable after the loop | `T` | `v.into_iter()` |
| `for x in ref v` | **Borrow iteration**: non-consuming; v still usable after the loop | `ref T` (value for Copy elements) | `v.iter()` |
| `for x in mut ref v` | **Mutable borrow iteration**: elements modifiable through x | `mut ref T` | `v.iter_mut()` |

- The three markers correspond to three **different semantics** (consume /
  read-only / modify), not equivalent forms; they do not violate P1
- **No implicit iteration**: `for` only accepts types explicitly implementing
  the iteration interface (see the `__iter__` row in §5); markers translate to
  named interface method calls — interface contracts, not syntax magic
- **Copy element rule**: in borrow iteration, Copy elements like
  `int`/`float`/`bool` give the loop variable a plain value (reading copies;
  no side effects; defined explicitly in the spec)
- **Conditional loops**: only `while <cond>:` + indented block is kept
  (condition-driven and collection-driven iteration are orthogonal
  semantics; Python does the same)
- Loop variables are immutable by default; `for mut x in ...` allows rebinding
  inside the body (consistent with `let mut`)

**Design rationale (AI-friendly)**: ownership is fully visible on the most
common construct — AI-generated loops won't silently consume a collection the
caller still uses; the compiler reports it immediately and suggests `ref`.

### D7. Mutability: Immutable by Default + Explicit `mut`

- **Bindings are immutable by default**: `let x = 5` cannot be reassigned;
  `let mut x = 5` can
- **Parameters are immutable by default**: `fn f(x: int)` cannot modify x
  inside; use `fn f(mut x: int)` when needed (`mut` before the name,
  consistent with `let mut`)
- **Field mutability follows the binding** (Rust model; no per-field `mut` in
  v1): `let mut s = ...` can mutate s's fields; `let s = ...` cannot — one
  less concept, simpler borrow checking
- **Two kinds of `mut` coexist at different levels** (explicitly
  distinguished in the docs):
  - Binding level: `let mut` / parameter `mut x` — modifies "name can be
    rebound"
  - Type level: `mut ref T` (see §6) — modifies "borrow is mutable"
- Standard library mutable containers (`List[T]`'s `push`/`set`, etc.)
  require a mutable binding or mutable borrow, enforced at compile time

**Design rationale (AI-friendly)**: immutability by default means AI-
generated code naturally has one less bug class (accidental mutation of shared
data); the position of `mut` is documentation — reading code = understanding
mutability boundaries.

---

## 4. Type System Goals

### 4.1 Base Types
- Numeric: `int` (arbitrary precision), `float` (IEEE 754), optional fixed-
  width types such as `i32/u32/i64` (TBD)
- `bool`, `str` (immutable, UTF-8), `bytes`
- Containers: `List[T]`, `Dict[K, V]`, `Set[T]`, `Tuple[...]`, `Range`,
  `Chan[T]` (see §7)

### 4.2 Key Semantic Decisions

| Topic | Decision | Notes |
|-------|----------|-------|
| Null values | **No null/nil literal**; use `Option[T]` | null is the #1 bug source in AI-generated code; `Option` forces handling |
| Error handling | **Explicit `Result[T, E]` returns**, no exceptions | Exceptions are implicit control flow; `Result` makes error paths visible |
| Equality | Value types = structural equality; reference types = reference equality; custom comparison requires explicit `equals()` | No `__eq__` magic; equality behavior is always predictable |
| Implicit conversions | **Always forbidden** | Even numeric promotion must be explicit (e.g. `float(x)`) |
| Generics | Square brackets, constraints supported (`T: Comparable`); generics are compile-time only | No runtime generic magic |
| Interfaces | Explicit `interface`; implementations must be declared explicitly (no implicit duck typing) | Consistent with "no magic methods" |
| Mutability | Immutable by default, explicit `mut` (see D7) | AI-generated code is safe by default; `mut` position is documentation |

### 4.3 Not Done
- ❌ No dependent types, no higher-kinded types, no type-level programming
  (a burden for AI generation)
- ❌ No runtime reflective type checks (limited `type(x)` queries, TBD)

---

## 5. Magic Method Policy

> This is the most important chapter of the whole document: it defines the
> concrete replacements for "banned magic methods".
> Principle: **any behavior is either built into the syntax or an explicit
> method call — there is no third state.**

| Python magic | Replacement in Sole | Example |
|--------------|---------------------|---------|
| `__add__` etc. operator overloading | **Operator overloading is banned for custom types**; operators apply only to standard library types. Custom type operations are always explicit methods | `a.add(b)` rather than `a + b` |
| `__eq__` / `__hash__` | Value types get structural equality automatically; reference types default to reference equality; custom comparison uses explicit `equals()` | `a.equals(b)` |
| `__str__` / `__repr__` | Explicit `.to_str()` method (interface convention from the stdlib, not automatically triggered) | `print(x.to_str())` |
| `__iter__` | Explicit implementation of the iteration interface; `for` only accepts that interface, and **ownership markers decide the iteration mode** (see D6): `into_iter()` / `iter()` / `iter_mut()` | `for x in ref items` |
| `__getitem__` / `__setitem__` | Only builtin containers support `[]`; custom types use explicit `get(k)` / `set(k, v)` methods | `cache.get(key)` |
| `__len__` | Explicit `.len()` method | `items.len()` |
| `__enter__` / `__exit__` | Resource management with `with` blocks + explicit `Resource { open(), close() }` interface | `with file` |
| `__call__` | Callable objects must hold a function explicitly as a field (`callable: Fn`); no implicit callable instances | `handler.call(args)` |
| `__getattr__` / dynamic attributes | **Does not exist**. No runtime attribute interception | — |

**Design rationale (AI-friendly):**
1. Hidden hooks are where AI hallucination thrives. When the model sees `a +
   b`, it cannot tell whether `a` implements `__add__`; behavior becomes
   unpredictable
2. Explicit method names make "reading code = understanding code"; reviewing
   AI-generated code becomes far cheaper
3. Clear semantic boundaries: operators do one thing, methods are methods

**Trade-off (honestly recorded):**
- Cost: expression syntax for custom types is more verbose
  (`matrix.multiply(other)` instead of `m1 * m2`)
- Mitigation: the stdlib provides common types like `Matrix`, keeping
  overloading convenience inside the stdlib; custom types get practical
  combinator methods

---

## 6. Memory & Ownership Model

> Author's decision: **ownership model, no GC**. GC pauses and
> nondeterminism are unacceptable.
> Hard constraint: **Rust's `&`, `&mut`, and `'a` lifetime symbols are NOT
> introduced** — ownership semantics remain, visual burden goes to zero.

### 6.1 Design: Ownership Without Symbols

| Need | Rust | Sole |
|------|------|------|
| Immutable borrow parameter | `fn f(x: &T)` | `fn f(x: ref T)` |
| Mutable borrow parameter | `fn f(x: &mut T)` | `fn f(x: mut ref T)` |
| Lifetime annotations | `fn f<'a>(x: &'a T) -> &'a T` | **Not needed**; the compiler infers that returned borrows originate from input borrows |
| Struct holding a reference | `struct S<'a> { x: &'a T }` | **Forbidden**; use `Shared[T]` or move ownership |
| Shared data | `Rc<T>` / `Arc<T>` | `Shared[T]` (stdlib, reference-counted) |
| String slices | `&str` | No slice borrows; `str` is a COW value type (see 6.3) |

### 6.2 Core Rules

1. **Move semantics by default**: assignment, passing, and returning move by
   default (non-Copy types), no symbols needed
2. **Copy types**: `int`/`float`/`bool` and other PODs copy automatically
   (built into the language; no `Copy` trait ceremony)
3. **Borrows with keywords**: `ref` / `mut ref` are plain keywords,
   consistent with postfix type style (`x: ref T`)
4. **Zero explicit lifetimes**: borrow regions are inferred by the compiler
   and bound to the lexical block that creates them; regions appear only in
   **error messages** (described in human-readable language: "this borrow
   outlives the block that created it")
5. **Structs never hold borrows**: for shared data, explicitly use
   `Shared[T]` (reference-counted, immutable sharing; `.clone()` explicitly
   increases the count)

### 6.3 `str` Design

- `str` is an **immutable, value-semantic, copy-on-write (COW)** type (like
  Swift's `String`), so no `&str` slices are needed
- Substringing returns a new value: `s.sub(start, end) -> str` (copies) or
  `s.slice_view(...) -> Slice[T]` (region-limited borrow view, later)
- Strings, the biggest consumers of borrows, thus naturally avoid borrow
  syntax

### 6.4 Design Rationale (AI-friendly)

1. **Borrow checking is the #1 source of AI generation failure in Rust**.
   Removing symbols and explicit annotations cuts away a huge failure surface
2. `ref`/`mut ref` are existing concepts (C# `ref`, Swift `inout`); AI
   analogies cost little, and models won't emit unfamiliar syntax
3. "Structs cannot hold borrows" is a **strong but simple** rule: the
   compiler can always give a concrete alternative (`Shared[T]` / move
   ownership), keeping errors actionable
4. No GC = no pauses, deterministic behavior, consistent with P2

### 6.5 Trade-offs (honestly recorded)

- Cost: the Rust pattern "struct holds a reference" needs a different style in
  Sole; ownership implementation is significantly harder than a GC
- Mitigation: such cases are a small share of business code; `Shared[T]`
  covers the vast majority
- Implementation risk**: the borrow checker is the hardest part of all
  milestones → in two steps: M1/M2 implement "move semantics + minimal borrow
  rule set", M3 completes it (see §10.3)

---

## 7. Concurrency Model

> Author's decision: **CSP-style coroutines + structured concurrency** — like
> Go's goroutine + channel, but with a task tree, scoped waiting, and
> cancellation propagation to eliminate "fire-and-forget leaks" and runaway
> background tasks (2026-08-11)

### 7.1 Model

- **Coroutines** (lightweight user-space tasks, M:N scheduled onto a few OS
  threads), not 1:1 threads
- **Communication via channels**: sending **moves** a value into the channel,
  receiving **moves** it out — perfectly aligned with the §6 ownership model:
  passing data across tasks = moving ownership, no shared mutable state,
  **data races are impossible at the type level** ("share memory by
  communicating")
- **Structured concurrency**: tasks form a tree; a parent scope must wait for
  all child tasks before exiting; early parent exit (return / error) →
  cancels all child tasks and propagates

### 7.2 Syntax (consistent with existing decisions)

| Concept | Sole syntax | Notes |
|---------|-------------|-------|
| Task group | `task_group:` + indented block | waits for all child tasks before block end; early exit → cancels children |
| Spawning | `go worker(x)` | spawns a child task in the **current task_group**; top level is implicitly in a group |
| Channel type | `ch: Chan[T]` | postfix type + square-bracket generics, consistent with D2/D4 |
| Creating a channel | `ch = Chan[int]()` | unbuffered; `Chan[int](10)` = buffer 10 |
| Send | `ch.send(v)` | **move semantics**: v's ownership enters the channel (except Copy types) |
| Receive | `v = ch.recv()` | returns `Option[T]` (None = channel closed / group cancelled) |
| Receive loop | `for v in ch` | recv one value per iteration; `Chan` is a handle type, the loop does not consume the handle; closed or cancelled → loop ends naturally |
| Close | `ch.close()` | after close, `send` returns an error `Result`; `recv` returns None after draining |

> Note: all channel operations are **explicit method calls**, no `<-`-style
> symbols — consistent with "no magic methods + no mysterious symbols".

### 7.3 Structured Concurrency Rules

1. `go` may only appear inside a `task_group`; the top-level script is
   automatically in an **implicit task_group** (auto-waits for all top-level
   tasks before program end)
2. **Child task lifetime ⊆ enclosing group's scope** → borrow checking and
   concurrency are naturally compatible: child tasks can only borrow data
   visible within the group, which the compiler can verify (structure is the
   benefactor of the borrow checker)
3. **Cancellation = closing**: early group exit → cancel child tasks →
   equivalent to closing the channels they depend on → blocked
   `recv`/`send` return immediately, `for` loops end naturally
4. **Cooperative scheduling**: switching happens only at channel operations
   and explicit `yield` (no preemption) → predictable behavior, consistent
   with P2
   - Cost (honestly recorded): CPU-heavy loops that don't yield can starve
     other tasks → explicit `yield` needed (stdlib convention)

### 7.4 Relationship to the Ownership Model (the biggest synergy in this design)

- send/recv = ownership moving across tasks → **no shared mutable state, data
  races impossible** (no locking mindset needed)
- `Shared[T]` (§6) is an **atomic reference count** (Arc-like), safe for
  sharing immutable data across tasks
- Mutable shared data (rare): stdlib `Mutex` with explicit `lock()`/`unlock()`
  (method calls, no syntax magic); can be deferred in v1

### 7.5 Not in v1

- ❌ `select` multiplexing (Go select) is not in v1 — wait for real need;
  `for` loops + cancellation cover most scenarios
- ❌ No direct exposure of OS threads (managed by the runtime scheduler)
- ❌ No escaping tasks (fire-and-forget) — the core promise of structured
  concurrency; no exceptions

### 7.6 Trade-offs (honestly recorded)

- Structured concurrency sacrifices Go's "free spawn": long-running background
  tasks must be explicitly organized (implicit top-level group + explicit
  group waiting)
- Cooperative scheduling: predictable but requires yield discipline;
  preemption will be reconsidered when real performance pressure appears
  (recorded in §11)

---

## 8. Standard Library Goals

- **Small and refined**: core library covers: collections, text, I/O, time,
  math, JSON, networking (HTTP), testing, concurrency primitives
- **Naming consistency**: the same operation has the same name everywhere
  (`len()`, `to_str()`, `parse()`); API guessability first
- **No implicit imports**: all imports explicit (no Python-style implicit
  magic globals)
- **Built-in testing primitives**: `test` blocks / `assert`, letting AI agents
  verify their own code
- Third-party package management: reserved (for the language's success, does
  not block v1)

---

## 9. Toolchain & AI Integration

### 9.1 Required Tools (by priority)
1. **Compiler/interpreter core**: fast startup, incremental compilation
   (< 100ms feedback loop)
2. **Formatter**: the official canonical format (gofmt-like). AI output never
   needs to agonize over style
3. **LSP**: completion, navigation, diagnostics. Both AI and humans rely on
   it for instant feedback
4. **Test runner**: integrated with the LSP
5. **Debugger** (later)

### 9.2 AI-Friendly Design
- **Error message spec**: every compile error must contain — `stable error
  code (E00xx lexical / E01xx parsing / E02xx evaluation / E03xx type
  checking)` + `location` + `cause` + `fix suggestion` + `correct example`
  (structured, AI-parseable)
- **Bilingual error messages**: English by default, `--lang zh` or the
  `SOLE_LANG=zh` env var switches to Chinese; errors = data (code +
  parameters), language is only a rendering layer (the `sole-diag` crate);
  keywords/identifiers remain English-only
- **Example library**: 2-3 golden examples per syntax feature, as AI
  "few-shot corpus"
- **Common error reference table**: a curated "AI common mistakes → correct
  form" document (manual initially, later mined from issues)
- **Machine-readable docs**: spec and API docs structured (compilable to
  JSON) for AI retrieval

---

## 10. Success Metrics

### 10.1 AI-Friendliness (quantifiable)
- [ ] ≥ 90% first-pass compile rate for AI-generated code on a fixed task set
  (e.g. HumanEval-style, 50 problems)
- [ ] ≥ 95% of compile errors directly usable by AI for fixing (no human
  interpretation needed)
- [ ] ≥ 99% of AI output passes lint after "format-only" corrections by the
  formatter

### 10.2 Human Experience
- [ ] Developers familiar with Python/TS can read the complete example set
  within 1 hour
- [ ] No "syntax friction" complaints after implementing a medium project
  (≥ 1000 lines) in this language

### 10.3 Milestones
| Phase | Content | Acceptance |
|-------|---------|------------|
| M1 ✅ | Lexer + parser + tree-walking interpreter | hello world, fibonacci, simple type checking (completed 2026-08-11: E03xx type-check error codes, AST Span locations) |
| M2 ✅ | Static type checker (complete, **including move semantics and minimal borrow rules**) | Type/borrow errors reported with locations (completed 2026-08-11: `List[T]`/`struct`/`interface`/`impl`, move semantics and borrow flow analysis, E04xx borrow error codes) |
| M3 | Bytecode VM (with **coroutine scheduler and channel runtime**; compiling to C/LLVM as needed later, completing the borrow checker) | Performance ≥ same order of magnitude as CPython |
| M4 | Standard library core + formatter + LSP | Can complete a real small project |
| M5 | AI benchmark toolchain | Achieve the §10.1 metrics |

**Implementation language & build**: all milestones in **Rust** (from M1 on).

---

## 11. Open Questions

> These answers will significantly shape the language; settle them one by one
> before entering M1 implementation.

1. ~~Language name~~ **Decided: Sole** (2026-08-11)
2. **Primary runtime scenario**: provisionally general purpose; will narrow
   when concrete needs appear (see §1.2); re-evaluate §8 stdlib priorities and
   §10.3 backend choice when narrowing
3. ~~Memory management~~ **Decided: ownership model, no GC** (2026-08-11);
   constraint: no Rust `&`/`&mut`/`'a` symbols → see §6
4. ~~Concurrency model~~ **Decided: CSP coroutines + structured concurrency**
   (2026-08-11) → see §7
5. ~~`for` loop form~~ **Decided: `for` + explicit iteration + ownership
   markers** (2026-08-11) → see D6
6. ~~Mutability~~ **Decided: immutable by default + explicit `mut`**
   (2026-08-11) → see D7
7. **Compile target**: bytecode VM as the default engine for the dev feedback
   loop (retained); whether to provide a native/compiled backend is decided
   after the scenario narrows

---

## 12. Changelog

| Date | Change | Reason |
|------|--------|--------|
| 2026-08-11 | v0.1 draft: 8 principles, 4 syntax decisions (D1-D4), magic method policy | Decided with the author |
| 2026-08-11 | Named **Sole** | Author's call; means "only"/"single way", echoing the design philosophy |
| 2026-08-11 | Memory model decided as **ownership (no GC)**, new §6; explicitly no Rust `&`/`&mut`/`'a` symbols; borrows use keywords `ref`/`mut ref`; `str` is a COW value type | Author's call: GC pauses and nondeterminism unacceptable; symbol style violates the "explicit and readable" syntax decisions |
| 2026-08-11 | Primary scenario decided as **scripting**: no build step, shebang direct execution, startup < 50ms; bytecode VM preferred backend for M3 | Author's call; affects runtime architecture and stdlib priorities |
| 2026-08-11 | Scenario changed to **provisional general purpose**: scripting's technical preferences (fast startup / shebang / bytecode VM) downgraded to soft preferences, no hard targets; stdlib priorities and compile backend decided when scenario narrows | Author had doubts about the scripting positioning; after re-discussion, decided not to bind a domain; the "correctness-first" identity of ownership + static types is unchanged |
| 2026-08-11 | Concurrency model decided as **CSP coroutines + structured concurrency**, new §7: `task_group` / `go` / `Chan[T]`; send/recv are ownership moves; cancellation = closing; cooperative scheduling; M3 includes coroutine scheduler and channel runtime | Author's call: Go-style free goroutines risk leaks and runaway tasks; structured concurrency is naturally compatible with ownership/borrow checking |
| 2026-08-11 | Loop form decided as **`for` + explicit iteration + ownership markers**, new D6: `for x in v` (move) / `ref v` (borrow) / `mut ref v` (mutable borrow); `while` remains the only conditional loop; `for` translates to explicit iteration interface calls | Author's call: ownership fully visible on loops; AI-generated loops won't silently consume collections still in use |
| 2026-08-11 | Mutability decided as **immutable by default + explicit `mut`**, new D7: `let mut` / parameter `mut x`; field mutability follows the binding; the two kinds of `mut` (binding-level and type-level) explicitly distinguished | Author's call: immutability by default means one less bug class in AI-generated code; all §11 semantic-layer questions now settled |
| 2026-08-11 | Implementation strategy settled: all milestones in **Rust** (from M1); compile/test handled by GitHub Actions CI; syntax examples unified to "colon + indentation" (fixed residual `{}` forms in the docs); `and`/`or`/`not` provisionally the boolean operators | Author's call: avoid double-implementation rework; iSH memory-constrained, no rustc |
| 2026-08-11 | Bilingual structured errors: new `sole-diag` crate; errors = stable codes (E00xx/E01xx/E02xx) + parameters, language is only a rendering layer; English by default, `--lang zh` / `SOLE_LANG` switch; keywords/identifiers stay English-only | Author's call: aligns with GOALS §9.2 "structured, AI-parseable" metric; English is friendlier for AI fixes |
| 2026-08-11 | **M1 complete**: full-node `Span` (line/column) on the `sole-parser` AST; new `typecheck` module in the `sole` crate (M1 simple static type checking: annotations/assignment/function signatures/operators/iteration, error code segment E03xx); `run_source` pipeline is now lex → parse → typecheck → eval; evaluation errors carry positions | Aligns with the M1 acceptance "simple type checking" and GOALS §9.2 error-location requirements |
| 2026-08-11 | Removed the "iSH memory-constrained, no local rustc, compile/test handled by GitHub Actions" notes: the dev environment now has ample resources; compilation/formatting/linting/testing run locally again (README and §10.3 updated) | Dev environment changed; old notes no longer apply |
| 2026-08-11 | **M2 complete**: type system extended (`List[T]`/`Struct`/`Interface`/`Ref`/`MutRef`, with interface subtype compatibility); new `struct`/`interface`/`impl` syntax with method calls (including borrowed `self`); move semantics and borrow checking (use-after-move / moves while borrowed / mutable borrow conflicts / borrow escape, error code segment E04xx); eval reworked to `Rc<RefCell>` cells for shared references; List literals/indexing/builtin methods; field assignment `obj.field = v` | Aligns with the M2 acceptance "static type checker (complete, with move semantics and minimal borrow rules)"; borrows are the minimal rule set — field-level borrows and borrow propagation supported, index borrows and fine-grained regions deferred to M3 |
