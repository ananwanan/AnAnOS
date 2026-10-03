# M1 物理内存管理

当前仍使用物理地址，MMU、数据缓存及指令缓存关闭，只有 CPU0 运行。
本阶段没有全局堆分配器，未引入 `alloc`、用户进程或虚拟地址空间。

## 所有权与发现

固件 DTB 经 `x0 -> x20 -> kernel_main(x0)` 传入。裸指针读取集中在
`kernel/src/boot_memory.rs`，依赖固件提供可读、不可并发修改的 DTB；魔数和
边界检查不能证明任意物理指针有效。接受 8 字节对齐、40 字节至 2 MiB、
位于首个 1 GiB 且不与内核冲突的 DTB。解析后再次确认 DTB 完整位于 RAM。
无效 DTB 不启用页分配器，不猜测板载 RAM 大小。

`kernel/src/memory/dtb.rs` 是无堆的纯解析器，主机测试使用合成 DTB：

- FDT v17 兼容格式，32/64 位大端 cells，直接 root memory 节点。
- 最多 16 个 RAM 范围、64 个保留范围、8 个动态池、32 层节点。
- header memreserve、identity `/reserved-memory`、`/chosen` 的 initrd。
- 禁用节点不贡献 RAM/保留区；不支持的地址转换和歧义返回错误。
- 动态保留池在内核/设备保留区加入后，选择符合大小、对齐和
  `alloc-ranges` 的连续 RAM，只保留请求大小。无合法位置时禁用分配器。

Raspberry Pi 官方 DTS 的 memory `reg` 是占位值，必须经过固件填充后使用。
动态 CMA 的允许范围可以覆盖前 1 GiB；不能把整个允许范围永久当成已占用
RAM。当前只为这些池划出物理空间，没有实现 CMA/DMA 分配服务。

页分配器额外排除：

| 区域 | 排除依据 |
| --- | --- |
| `[0, 0x0020_0000)` | 固件启动数据、UART bootloader 及其栈；保守保留 |
| `__kernel_start..__kernel_end` | linker 定义，包含 BSS 位图及完整 64 KiB 栈 |
| DTB 本身 | header totalsize |
| `[0xFC00_0000, 0x1_0000_0000)` | BCM2711 低地址外设别名、GIC、本地外设 |
| VideoCore RAM | Property Mailbox `Get VC memory` 返回的物理 base/size |
| framebuffer | 固件实际返回的物理地址与 size |

VC 内存查询失败时不启用页分配器。Framebuffer 失败仍可通过 UART 进行诊断；
VC 内存查询与 DTB 保留策略不依赖成功安装显示控制台。

## 物理页接口

`PageAllocator<const WORDS: usize>` 使用 managed/free 两个位图，构造为全零
静态 BSS，初始化原地执行。内核实例覆盖 16 GiB 物理地址，位图占 1 MiB。
这是地址范围上限，实际内存和空洞来自 DTB；超出上限返回错误。

页大小 4096 字节。RAM 边界向内取整，保留边界向外取整。物理页 0 禁止分配，
重叠保留区去重。拒绝未初始化、重复初始化、耗尽、非页对齐释放、保留区/
空洞释放及重复释放。`total_pages` 是排除所有保留区后的托管页数。

纯位图分配器只管理所有权，不访问页内容。初始化自测通过前不开放公共分配
接口，自测失败不会向后续子系统提供页。内核运行时接口：

```rust
allocate_zeroed_page() -> Result<usize, PageError>
unsafe free_page(address: usize) -> Result<(), PageError>
stats() -> PageStats
```

调用者拥有返回页；释放前必须结束所有引用、映射和 DMA 使用。清零采用页对齐
u64 volatile stores。元数据访问使用 CPU0 IRQ 临界区，不使用 MMU-off 环境下
不可靠的独占读写自旋锁。这个临界区不是 SMP 锁，启用其他 CPU 前必须替换。
`aarch64-unknown-none` 的 strict-align 代码生成也必须保持。

## 验证

完整开发机检查：

```powershell
.\scripts\check_windows.ps1
```

主机测试显式覆盖默认裸机 target，仅编译纯逻辑库：

```powershell
cargo test -p kernel --lib --target x86_64-pc-windows-msvc
```

启动时自动执行两页分配、完整清零检查、写入读取、释放、地址复用与重新清零。
板上应出现以下诊断且之后 timer tick 持续增长：

```text
[ OK ] PAGE ALLOC/FREE/ZERO SELF-TEST
[ OK ] PHYSICAL PAGE ALLOCATOR
Pages total : ...
Pages free  : ...
TIMER TICK: ...
```

`test::test_exception_simd_context()` 是可选板上 BRK 诊断，验证 q0/q1/q31 两个
64 位 lane 以及 FPCR/FPSR；它不会默认运行，也不代替持续 IRQ 验证。
bootloader 本次修复后须使用新镜像才能验证 UART ACK 排空和 DTB 跳转约束。
可移动盘安装/上传按 AGENTS.md 手动进行，不由检查脚本执行。

没有观察板上 UART/HDMI 时，以上只能称为主机测试、编译与镜像验证。

资料：

- [DTSpec FDT layout](https://devicetree-specification.readthedocs.io/en/latest/chapter5-flattened-format.html)
- [DTSpec memory and reserved-memory](https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html)
- [Raspberry Pi Property Mailbox](https://github.com/raspberrypi/firmware/wiki/Mailbox-property-interface)
- [Pi 4 SoC DTS](https://github.com/raspberrypi/linux/blob/rpi-6.12.y/arch/arm/boot/dts/broadcom/bcm2711.dtsi)
