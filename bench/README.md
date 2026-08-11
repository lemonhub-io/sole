# 性能基准（M3 VM 版）

对比 CPython 3.x，release 构建，取 3 次最优值。本机：Windows + 单次测量有波动，多次取最小。

## 结果

| 程序 | Sole | CPython | 比值 (Sole/CPython) |
|---|---|---|---|
| fib(28) + sum_loop(1e6) | ~587 ms | ~159 ms | ~3.7x |
| sum_loop(1e6) 单独 | ~165 ms | ~122 ms | ~1.3x |
| fib(28) 单独 | ~434 ms | ~34 ms | ~12.8x |

## 结论

- 循环/算术路径已达 CPython 同量级（~1.3x）。
- fib 递归是主要瓶颈（12.8x）：fib(28) ≈ 83 万次调用 ≈ 700ns/调用，开销集中在：
  1. 每次 Call 分配 `Rc<RefCell<Value>>` locals
  2. 每条指令 `func.code[ip].clone()`（含数据字段）
  3. 指令分派

## 优化方向（未实施）

1. **Frame.locals 改 `Vec<Value>`**（去掉 Rc<RefCell> 装箱）：
   普通 ref 借用运行时用值拷贝即可（静态借用检查已保证借用期不写/不移动）；
   仅 `mut ref` 写回需要 cell。改动面：Frame 结构 + 所有 locals 访问点。
2. **指令引用匹配**：`let instr = &func.code[ip]` 替代逐条 clone。
   已尝试，因正则批量替换误伤 `*n as usize` 解引用而回退；需逐分支手改解引用。

## 复现

```
cargo build --release -p sole
.\target\release\sole.exe run bench/fib_sum.sole
python bench/fib_sum.py
```
