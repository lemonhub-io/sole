# Sole

> 一门面向"人类与 AI 协作编程"的静态类型、缩进式通用语言。
> 设计文档: [docs/GOALS.md](docs/GOALS.md)

## 状态

**M1(词法 + 解析 + 树遍历解释器 + 简单类型检查)已完成。**
**M2(完整静态类型检查 + 移动语义 + 借用检查 + List/struct/interface)已完成。**

当前支持:

- 缩进式语法、`fn` / `let` / 赋值 / `if-else` / `while` / `for` / `return`
- 后置类型标注与方括号泛型(`List[int]` 等)
- 完整静态类型检查(标注一致性、赋值、函数签名、运算符、循环迭代、列表元素;错误码 E03xx)
- 移动语义(非 Copy 值 move 后使用报错)与借用检查(use-after-move、借用期间移动、
  可变借用冲突、借用逃逸;错误码 E04xx)
- 集合类型 `List[T]`(字面量 `[1, 2]`、索引、`len`/`push`/`get`/`set`)
- 结构体 `struct` / 接口 `interface` / 实现 `impl` 与方法调用(含 `self` 借用)
- 内置函数 `print`、`range`
- 可变性检查(`let mut` 才能重新赋值)、错误定位(词法/解析/类型检查/求值错误均带行列)
- 双语错误信息(`--lang en|zh` 或 `SOLE_LANG` 环境变量)

## 快速开始(在任意有 Rust 的机器上)

```sh
cargo run --bin sole -- run examples/hello.sole
cargo run --bin sole -- run examples/fib.sole
cargo run --bin sole -- run --lang zh examples/hello.sole   # 错误信息中文
cargo test --workspace
```

错误信息默认英文(带稳定错误码,如 `[E0201]`),用 `--lang zh` 或环境变量 `SOLE_LANG=zh` 切换中文。

## 开发工作流

```sh
# 本地验证
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 暂定 / 未实现(M1/M2 笔记)

- `and` / `or` / `not` 为**暂定**布尔运算符关键字
- `int` 暂用 `i64`(设计目标为任意精度,GOALS §4.1)
- 整数除法暂为截断;truthiness 暂为 Python 风格(暂定)
- 借用检查为 M2 最小规则集(变量级 + 字段级借用、借用传播);M3 补全细粒度与索引借用
- 未实现: 完整泛型(自定义泛型类型与约束)、`Dict`/`Set`/`Tuple` 等其余集合、
  并发原语(`task_group` / `go` / `Chan`,关键字已保留)
- `ref` / `mut ref` 有完整借用检查,但无运行时借用逃逸检测(由静态检查保证)
