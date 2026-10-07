# 性能基准(M3 VM 版)

对比 CPython 3.x,release 构建,取多次最优值。

## 结果(2026-08-16 第三轮优化后,同日同机对比)

本机(ARM64 虚拟化容器,时钟波动大,本轮绝对值整体偏慢 ~8x,
故与同日同机测得的基线 6be9b29 和 CPython 比值为准):

| 程序 | Sole(本轮) | Sole(基线) | CPython | Sole/CPython |
|---|---|---|---|---|
| fib(28) + sum_loop(1e6) | ~2.6 s | ~4.2 s | ~1.5 s | ~1.7x |
| sum_loop(1e6) 单独 | ~0.60 s | ~0.67 s | ~0.91 s | ~0.66x(反超) |
| fib(28) 单独 | ~2.2 s | ~3.4 s | ~1.0 s | ~2.2x |

> 初版(2026-08-13):全程序 ~3.7x CPython(Windows 单机测量);
> 本机第一轮优化后:全程序 ~1.3-1.8x;
> 本机第二轮优化后:全程序 ~1.0x,两项子基准均反超 CPython;
> 本机第三轮(技术债第二轮,P1):较本轮开始前基线 fib 提速 ~35%,
> 循环路径保持反超 CPython;fib(纯递归调用路径)仍落后,残余开销
> 在按帧调用成本(Frame 压栈/弹栈 + locals 区扩缩)与值 clone/drop。

## 结论

- **M3 验收达标**:循环路径(sum_loop)稳定反超 CPython(~0.66x);
  调用密集路径(fib)经第三轮分派优化后较基线提速 ~35%,仍落后
  CPython,继续作为 P 系列优化的目标。
- 残余开销:按帧调用成本与值 clone/drop;`Value` 已缩至 16B。

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

第三轮(2026-08-16,本轮):
9. **帧切换检测移出热路径**:分派循环不再逐指令 `frames.last()` +
   函数号比较;`func`/`code` 缓存仅在 Call/CallMethod/Return/RetUnit
   四个真正压弹帧的指令臂内刷新。
10. **`Binary` 快路径直连**:i64 算术直接用 `checked_*` 运算 +
    溢出晋升 `Big`,不再绕行 `IntVal` 包装/解包;`cmp_op` 的
    `(Int, Int)` 分支同理直接比较。
11. **方法分派哈希表 + `Value` 16B**(P4/P2 同轮落地):`Rc<String>`
    瘦指针化、`Range` 装箱,值拷贝成本进一步下降。

## 复现

```
cargo build --release -p sole
./target/release/sole run bench/fib_sum.sole
python3 bench/fib_sum.py
```
