# Sole

> 一门面向"人类与 AI 协作编程"的静态类型、缩进式通用语言。
> 设计文档: [docs/GOALS.md](docs/GOALS.md)

## 状态

**M1(词法 + 解析 + 树遍历解释器)进行中。**

当前支持:

- 缩进式语法、`fn` / `let` / 赋值 / `if-else` / `while` / `for` / `return`
- 后置类型标注与方括号泛型(**仅解析**,类型检查在 M2)
- 内置函数 `print`、`range`
- 可变性检查(`let mut` 才能重新赋值)、缩进/词法错误定位

## 快速开始(在任意有 Rust 的机器上)

```sh
cargo run --bin sole -- run examples/hello.sole
cargo run --bin sole -- run examples/fib.sole
cargo test --workspace
```

## 开发工作流

开发机是内存受限的 iSH 环境,**不运行 rustc** —— 编译、格式化、lint、测试全部由
GitHub Actions 承担(每次 push 自动运行,见 `.github/workflows/ci.yml`)。

```sh
# 本地: 编辑 → 提交 → push
git add -A
git commit -m "..."
git push
# CI 自动运行: fmt --check / clippy -D warnings / cargo test
```

## 暂定 / 未实现(M1 笔记)

- `and` / `or` / `not` 为**暂定**布尔运算符关键字
- `int` 暂用 `i64`(设计目标为任意精度,GOALS §4.1)
- 整数除法暂为截断;truthiness 暂为 Python 风格(暂定)
- 未实现: 类型检查器(仅解析)、借用检查、集合类型(`List` 等)、结构体/接口、
  并发原语(`task_group` / `go` / `Chan`,关键字已保留)
- `ref` / `mut ref` 类型可解析但运行时无借用语义
