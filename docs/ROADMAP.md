# AnanOS 实施与验收路线

目标是 Raspberry Pi 4B 上的 Unix/POSIX 风格操作系统，最终能运行本机
C/C++/Rust 工具链。按 AGENTS.md 的依赖顺序推进；代码完成、主机测试通过和
实机验收分别记录，不能互相替代。

## M1 物理内存与启动安全

实现范围：

- 保留 EL1h、CPU0、Mini UART、GICv2、Generic Physical Timer 的现有启动路径。
- 异常帧保存全部通用寄存器、SIMD 寄存器、FPCR/FPSR。
- DTB 解析 RAM、保留区、initrd 和动态保留池约束。
- 4 KiB 物理页分配、清零、回收及错误检查。
- 连续页所有权、bootloader DTB 重定位和 1 MiB 可复用内核堆。
- CPU0 IRQ 临界区和控制台重入处理。
- 可复用的主机测试、镜像构建和板上诊断。

原物理页基础快照的 35 项主机测试和镜像检查记录见
[M1 验证记录](validation/M1.md)。合并后的 DTB/堆版本须使用统一检查脚本
重新验证，最新结果见该记录的合并验证部分；实机 UART、
HDMI、持续 timer IRQ、页清零/回收及可选 SIMD BRK 诊断仍须在板上观察。
内存错误会禁用内存相关子系统，并保留已有设备诊断路径。

## 后续里程碑与依赖

| 阶段 | 交付能力 | 通过条件 |
| --- | --- | --- |
| M2 | 页表、MMU、内存属性 | 分配/释放压力测试；RAM、MMIO、mailbox、framebuffer 属性明确；实机 IRQ 与设备访问不退化 |
| M3 | EL0、地址空间、上下文切换、最小 syscall | Gate A：EL0 程序通过 SVC 输出并退出；非法用户指针可控失败；内核隔离有效 |
| M4 | 进程、静态 ELF64、initramfs、VFS/FD | 独立 ELF 加载；argv/envp；stdin/out/err；文件与目录；exec/wait；多进程资源回收 |
| M5 | 用户 VM、CRT、libc、sysroot、cross Binutils/GCC | Gate B/C：静态 C hello、malloc/free、文件、时间和多进程程序在板上运行 |
| M6 | TLS、线程、信号、libstdc++、Rust core/alloc/std | Gate D/E：C++ 容器/异常/线程和普通 Rust std 程序可交叉编译并执行 |
| M7 | 可写持久存储、shell、构建工具、本机编译器 | Gate F/G/H/I：板上 gcc/g++/rustc 编译并执行程序，Cargo 离线构建 |

动态链接在静态程序稳定后引入。在线 Cargo 另依赖网络、DNS、TLS 和证书，
不作为早期离线工具链验收条件。ABI 在 EL0/syscall 实现时集中写入 `docs/abi/`；
当前不发布固定 AnanOS target triple，不把 Linux syscall 或内核 Rust 类型
直接当成公开 ABI。

## 每个里程碑的执行规则

1. 读取实际代码及相关设计文档，检查当前分支和工作区。
2. 完成依赖最低的可测试纵向切片，记录资源所有权及失败路径。
3. 运行格式、workspace、相关包、主机逻辑测试及镜像检查。
4. 对真实硬件相关行为保留诊断，记录板上证据后才标记硬件通过。
5. 同步能力状态和下一阶段入口，不把后续 libc/toolchain 策略塞进内核。

M2 页表/MMU 基础代码已实现：可选 EL1 身份映射、4 KiB 页及大块映射、
W^X、no-map 空洞、Normal NC/Device 属性、页表资源回收和启动自测。
默认构建仍关闭 MMU；`-EnableMmu` 构建独立镜像，缓存继续关闭。
地址布局、mailbox 一致性约束和手动上板步骤见 [mmu.md](mmu.md)，
开发机验证见 [M2 记录](validation/M2.md)。M1/M2 板上验收仍未完成；
M3 的 Gate A 基础代码也已实现：独立 TTBR0/用户页、EL0t、SVC write/exit、
完整上下文返回、用户故障/栈保护页、EL0 timer 超时和资源回收。
`-EnableUserspace` 构建单独镜像并隐式启用 MMU，详见 [userspace.md](userspace.md)
和 [临时 ABI](abi/README.md)。实机 Gate A 验收仍待 UART 证据；
M3 单独镜像保留这些诊断；活跃页表修改和缓存开启仍未实现。

M4 `syscall -> 文件系统` 基础代码已实现：静态 ELF64 独立程序、Unix 入口栈、
四进程轮转、timer IRQ 保存上下文、spawn/exec/wait、进程 FD/cwd、只读 USTAR
initramfs、可写 RAM 文件/目录和常用文件 syscall。`-EnableFilesystem` 隐式
启用 MMU/EL0，构建 `kernel8-fs.img`，先跑前序诊断再跑 ELF 文件往返/exec/wait
和资源回收验收。主机及模拟证据见 [M4 记录](validation/M4.md)，实现边界见
[filesystem.md](filesystem.md)。M1-M4 实机验收均须真实 UART/HDMI 证据；
持久存储、pipes、libc/sysroot 和 hosted toolchain 继续按后续依赖推进。
