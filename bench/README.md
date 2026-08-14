# 性能基准(M3 VM 版)

对比 CPython 3.x,release 构建,取多次最优值。

## 结果(2026-08-14 优化后)

本机(ARM64 虚拟化容器,时钟波动大):Sole/CPython 交替测量取各自最优:

| 程序 | Sole | CPython | 比值 (Sole/CPython) |
|---|---|---|---|
| fib(28) + sum_loop(1e6) | ~400-450 ms | ~230-310 ms | ~1.3-1.8x |
| sum_loop(1e6) 单独 | ~125 ms | ~189 ms | ~0.7x(反超) |
| fib(28) 单独 | ~263 ms | ~217 ms | ~1.2x |

> 优化前(2026-08-13 初版):全程序 ~587ms vs CPython ~159ms(~3.7x,Windows 单机测量);
> 本机优化前实测全程序 1233ms vs CPython 280ms(~4.4x)。

## 结论

- **M3 验收达标**:全程序与 CPython 同量级(~1.3-1.8x),循环/算术路径已反超(~0.7x)。
- 递归(fib)仍是最慢路径(~1.2x),瓶颈在每次调用的 locals 分配与栈帧建立。

## 已实施的优化(2026-08-14)

1. **指令引用分派**:`let instr = &func.code[ip]` 替代逐条 `clone()`,不再为每条指令复制枚举。
2. **locals 值语义 + 惰性借用 cell**:`Frame.locals` 改 `Vec<Value>`,仅在被 `ref`/`mut ref`
   借用的槽位才创建 `Rc<RefCell<Value>>` cell;调用不再为每个局部变量分配 cell。
3. **`Value` 缩至 24B**(原 56B):`Str` 用 `Rc<str>`(字符串表同改,`PushStr` 零分配)、
   `Struct` 装箱为 `Box<StructVal>`;所有栈上 push/pop/局部拷贝减半。
4. **int 算术快路径**:`Binary` 臂内联 `(Int, Int)` 四则运算,`binary()` 改传引用,
   避免按值传参(ARM64 上 56B 结构体走内存)。
5. **run-until-block 调度**:任务一次运行到阻塞/yield/完成才交还调度器,
   消除每条指令的调度器往返(函数调用 prologue/epilogue、Rc 引用计数、循环检查);
   同时与 GOALS §7.3"协作式调度:仅在通道操作与显式 yield 处切换"对齐。

## 复现

```
cargo build --release -p sole
./target/release/sole run bench/fib_sum.sole
python3 bench/fib_sum.py
```
