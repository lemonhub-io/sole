# 技术债务清理第一轮(正确性优先)设计

日期: 2026-08-15 · 状态: 已批准(用户一次性交付)

## 背景

GOALS/README 记录了多处"诚实记录"的设计妥协与文档-实现不一致。本轮以**正确性优先**为
主线: 先清"文档声称但实现不达标"的债,低成本即修,高成本记入债务清单。

## 范围

### 1. VM 运行时错误定位(行号表)

- `Function` 增加 `lines: Vec<u32>`,与 `code` 一一对应(每条指令一个行号)
- 编译器: 新增 `CodeBuf { code, lines }` 包装(附 `push(instr, line)` / `len()` /
  `patch(at, instr)`),所有指令发射点同步记录 `expr.span().line` / `stmt.span().line`;
  合成指令(Halt/Return/RetUnit)行号 0
- VM: `Runtime::line_at(task)` = `lines[frames.last().func][task.ip-1]`(ip 取指令后已 +1);
  新增 `line: u32` 参数贯穿 8 个方法分发 helper(str/dict/set/tuple/option/result/list/chan);
  ~16 处 `err(msg, 0, 0)` 全部替换为带行号
- 运行时零开销: 行号只在出错路径读取;方法分发每调用多一次数组读(可接受)
- **取舍**: 只给行号不给列(列需 +8B/指令,收益低);README"求值错误均带行列"
  微调为"求值错误带行号,静态错误带行列"
- 测试: 除零、索引越界、UnknownMethod、unwrap(None/Err)、assert 失败均断言含行号

### 2. `-x` 专用指令

- 新增 `Instr::Neg`,编译器 `UnOp::Neg` 直接发射(push expr + Neg),废除 `0 - x` hack
  (每处取反 3 指令 → 2 指令,顺带微优)
- VM: Int 用 `wrapping_neg`(与 `0 - x` 的 wrapping_sub 语义一致),Float 取反,
  其它类型运行时错误兜底(类型检查已保证不会发生)
- 回归: 既有 `negative_literal_in_call_args` 测试保持

### 3. mojibake 注释

- 8 处 `闂?`(UTF-8 截断损坏,typecheck.rs 3 处 + vm.rs 5 处)替换为正常中文注释;
  仅注释文本,不动代码语义

### 4. 全面核对(文档声称 vs 实现)

- 错误码段归属(E00xx 词法 / E01xx 解析 / E02xx 求值 / E03xx 类型检查 / E04xx 借用)
- `for` 迭代机制(D6 声称"翻译为显式迭代接口调用")—— 若仅支持内建容器,
  文档降级(诚实记录)
- Json 与 `None` 可比较、`and/or/not`、方法集、模块、CLI、LSP、formatter、bench
  等已实测项抽查
- 处置原则: 低成本缺口本轮即修;高成本缺口(任意精度 int、哈希容器、索引借用、
  select/Shared/Mutex、LSP 增量/ hover、CLI 传参、自赋值、超大文件拆分等)
  记入新文档 `docs/DEBT.md`(编号 + 状态),GOALS 加链接

## 交付物

- crates/sole/src/compiler.rs — CodeBuf、Function.lines、Instr::Neg
- crates/sole/src/vm.rs — lines 查表、line 参数、mojibake
- crates/sole/src/typecheck.rs — mojibake
- crates/sole/src/lib.rs — 行号断言测试
- docs/DEBT.md、docs/GOALS.md 变更记录、README 双语微调

## 验证

- `cargo test --workspace` 全绿(既有 124 + 新增行号断言)
- `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings`
- json_tool 41 用例全绿
