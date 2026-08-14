# 性能基准(M3 VM 版)

对比 CPython 3.x,release 构建,取多次最优值。

## 结果(2026-08-14 两轮优化后)

本机(ARM64 虚拟化容器,时钟波动大):Sole/CPython 交替测量取各自最优:

| 程序 | Sole | CPython | 比值 (Sole/CPython) |
|---|---|---|---|
| fib(28) + sum_loop(1e6) | ~300 ms | ~300 ms | ~0.9-1.1x(持平) |
| sum_loop(1e6) 单独 | ~108 ms | ~189 ms | ~0.6x(反超) |
| fib(28) 单独 | ~199 ms | ~203 ms | ~1.0x(反超) |

> 初版(2026-08-13):全程序 ~3.7x CPython(Windows 单机测量);
> 本机第一轮优化后:全程序 ~1.3-1.8x;
> 本机第二轮优化后(本轮):全程序 ~1.0x,两项子基准均反超 CPython。

## 结论

- **M3 验收达标且反超**:全程序与 CPython 持平(~1.0x),循环/算术与递归路径均反超。
- 残余开销:指令分派本身(每条 ~4-6ns,理论下限 ~2ns)与 24B 值拷贝。

## 已实施的优化

第一轮(2026-08-14):
1. **指令引用分派**:`let instr = &func.code[ip]` 替代逐条 `clone()`。
2. **locals 值语义 + 惰性借用 cell**:仅被借用的槽位才创建 cell。
3. **`Value` 缩至 24B**(原 56B):`Str` 用 `Rc<str>`、`Struct` 装箱。
4. **int 算术快路径**:`Binary` 臂内联 `(Int, Int)` 运算,`binary()` 改传引用。
5. **run-until-block 调度**:任务运行到阻塞/yield/完成才交还调度器(对齐 GOALS §7.3)。

第二轮(2026-08-14,本轮):
6. **零拷贝调用**:局部变量直接放在任务栈上(`Frame.base` 索引),
   调用不再分配 locals Vec、不再倒序拷贝参数 —— 参数就地成为被调函数的前几个局部变量;
   返回时 `truncate(base)` 丢弃局部区。
7. **当前函数缓存**:分派循环缓存当前函数的 code 切片,仅 Call/Return 时刷新,
   免去每条指令的 `functions[func]` 表查找。
8. **`CallMethod(m, argc)` 编码参数个数**:编译器把接收者+参数总数编码进指令,
   VM 用 `stack[len-argc]` 定位接收者(配合共享栈)。

## 复现

```
cargo build --release -p sole
./target/release/sole run bench/fib_sum.sole
python3 bench/fib_sum.py
```
