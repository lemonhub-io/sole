# Sole

> 一门面向"人类与 AI 协作编程"的静态类型、缩进式通用语言。
> 设计文档: [docs/GOALS.md](docs/GOALS.md)

## 状态

**M1(词法 + 解析 + 树遍历解释器 + 简单类型检查)已完成。**
**M2(完整静态类型检查 + 移动语义 + 借用检查 + List/struct/interface)已完成。**
**M3(字节码 VM + 协程调度器 + 通道运行时)已完成。**
**M4(标准库核心 + formatter + LSP + 真实小项目验收)已完成。**

当前支持:

- 缩进式语法、`fn` / `let` / 赋值 / `if-else` / `while` / `for` / `return` /
  布尔运算符 `and` / `or` / `not`
- 后置类型标注与方括号泛型(`List[int]` 等)
- 完整静态类型检查(标注一致性、赋值、函数签名、运算符、循环迭代、列表元素;错误码 E03xx)
- 移动语义(非 Copy 值 move 后使用报错)与借用检查(use-after-move、借用期间移动、
  可变借用冲突、借用逃逸;错误码 E04xx)
- 集合类型 `List[T]`(字面量 `[1, 2]`、索引、`len`/`push`/`get`/`set`/`contains`)
- 结构体 `struct` / 接口 `interface` / 实现 `impl` 与方法调用(含 `self` 借用)
- 显式错误处理: `Option[T]`(`Some`/`None`、`is_some`/`is_none`/`unwrap`)与
  `Result[T, E]`(`Ok`/`Err`、`is_ok`/`is_err`/`unwrap`/`unwrap_err`)
- 泛型函数:`fn max[T: Comparable](a: T, b: T) -> T`,调用点实例化(约束 E0320)
- 其余集合:`Dict[K, V]`(索引、`len`/`get`/`set`/`contains`/`remove`/`keys`/`values`)、
  `Set[T]`(`len`/`add`/`contains`/`remove`,元素唯一)、`Tuple[...]`(索引、`len`)
- `str` 方法集:`len`/`sub`/`split`/`join`/`contains`/`starts_with`/`ends_with`/
  `trim`/`to_str`/`to_int`/`to_float`(`to_int`/`to_float` 返回 `Result`)
- 测试原语:`test` 块 + `assert` 语句(E0221),`sole test <file>` 运行
- 模块机制:`import foo` / `from foo import a, b`(多文件加载、循环去重、前缀重写)
- 标准库内建(全局可用):
  - IO:`read_to_str(path)` / `write(path, s)`(返回 `Result`)
  - 时间:`clock()`(毫秒)/ `sleep(ms)`
  - 数学:`abs`/`floor`/`ceil`/`round`/`sqrt`/`pow`
  - JSON:`json_encode(v)` / `json_decode(s)` + 动态 `Json` 类型(可索引、
    与 `None` 可比较、`len`/`contains`/`keys`/`is_int`/`is_str`/`to_str`)
- 字节码 VM:AST → 字节码(compiler + stack VM),locals 值语义 + 惰性借用 cell,
  `&Instr` 引用分派,run-until-block 协作调度(性能与 CPython 同量级,见 `bench/`)
- 并发原语:协程(`go`)、`task_group`(结构化并发:作用域等待 + 取消传播)、
  通道 `Chan[T]`(`send`/`recv`/`close`、缓冲/无缓冲、`for v in ch` 接收循环、`yield`)
- formatter:官方唯一格式(类 gofmt),`sole fmt <file|dir>` / `sole fmt --check`
- LSP server:`sole lsp`(full-sync 诊断、跳转定义、补全;复用类型检查器双语渲染)
- 可变性检查(`let mut` 才能重新赋值)、错误定位(词法/解析/类型检查/求值错误均带行列)
- 双语错误信息(`--lang en|zh` 或 `SOLE_LANG` 环境变量)

## 快速开始(在任意有 Rust 的机器上)

```sh
cargo run --bin sole -- run examples/hello.sole
cargo run --bin sole -- run examples/fib.sole
cargo run --bin sole -- run examples/list.sole       # List + 借用(M2)
cargo run --bin sole -- run examples/borrow.sole     # ref / mut ref(M2)
cargo run --bin sole -- run examples/shapes.sole     # struct / interface(M2)
cargo run --bin sole -- run examples/option.sole     # Option / Result(M4)
cargo run --bin sole -- run examples/collections.sole # Dict / Set / Tuple(M4)
cargo run --bin sole -- run examples/generics.sole   # 泛型函数(M4)
cargo run --bin sole -- run examples/str_methods.sole # str 方法集(M4)
cargo run --bin sole -- run examples/json_usage.sole  # 标准库 JSON(M4)
cargo run --bin sole -- run examples/concurrency.sole # go / task_group / Chan(M3)
cargo run --bin sole -- test examples/phase0.sole    # 运行 test 块(M4)
cargo run --bin sole -- fmt --check examples         # formatter 检查(M4)
cargo run --bin sole -- run --lang zh examples/hello.sole   # 错误信息中文
cargo run --bin sole -- run bench/fib_sum.sole       # 性能基准(M3,对比 CPython)
cargo test --workspace
```

真实小项目(M4 验收):`examples/projects/json_tool` —— 一个 JSON 命令行工具
(多文件模块 + 标准库 + 41 个 test 用例):

```sh
cargo run --bin sole -- test examples/projects/json_tool/main.sole
```

注:CLI 暂不支持向脚本传参,演示通过 `sole test` 驱动 `main(args)`(留待 M5)。

错误信息默认英文(带稳定错误码,如 `[E0201]`),用 `--lang zh` 或环境变量 `SOLE_LANG=zh` 切换中文。

## 开发工作流

```sh
# 本地验证
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 暂定 / 未实现

- `int` 暂用 `i64`(设计目标为任意精度,GOALS §4.1)
- 整数除法暂为截断;truthiness 暂为 Python 风格(暂定)
- 借用检查为最小规则集(变量级 + 字段级借用、借用传播);细粒度区域与索引借用未实现
- 未实现: `select` 多路复用、`Shared[T]` / `Mutex`(GOALS §7.5)、自定义泛型类型/接口
  (D9 明确不做)
- `ref` / `mut ref` 有完整借用检查,但无运行时借用逃逸检测(由静态检查保证)
- formatter 丢弃注释(词法器不保留);LSP 的 hover 与增量同步未实现(D14 诚实记录)
