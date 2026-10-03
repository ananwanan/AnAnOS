# M4: syscall 到文件系统

`filesystem` feature 隐式启用 `userspace` 和 `mmu`。默认、MMU-only 和 M3
镜像继续保留；CPU0 执行，其他核心停在 WFE，数据和指令缓存仍关闭。
本阶段实现静态 ELF 进程、文件 syscall、只读 initramfs 和可写 RAM FS。
Cargo/主机测试、模拟执行与树莓派实机验收分别记录。

## 启动与资源所有权

先执行现有 M1/M2/M3 自测和 IRQ preflight，再把内嵌 USTAR 挂载到 `/init`，
从 `/init/bin/init` 加载独立链接的 ELF64。`/` 和 `/tmp` 可写。USTAR 校验
头、校验和、路径、长度和结束块，拒绝链接、路径逃逸和未知扩展；安装失败
回滚整个挂载。文件内容引用内核 rodata 中的归档，不复制到 RAM 文件池。

ELF 只接受 little-endian AArch64 静态 `ET_EXEC`，校验完整 program headers、
PT_LOAD、溢出、地址窗口、页对齐、W^X、入口和容量。不同段不得共享页。
各段拥有私有清零物理页，按文件内容复制，BSS 和页尾保持零，映射 RX/R/RW。
保留 M3 的上下栈保护页；初始栈采用 argc/argv/NULL/envp/NULL/auxv，见
[ABI](abi/README.md)。动态链接、PIE、TLS 不在本阶段。

最多四个进程同时存在，PID 不复用。spawn 继承 cwd 和描述符（共享 open
description 与偏移），不复制父进程内存。exec 保留 PID/cwd/FD，先在第五个
暂存槽离线构造完整新地址空间；失败保留旧程序，成功在 EL1 切回内核根并
完成全 TLB invalidate 后替换和回收旧页。退出、用户故障和超时也先切回
内核根，再回收用户页/页表/FD。僵尸只保留 wait 状态；wait 结果复制成功
后才收割 PID。父进程退出时子进程成为孤儿，退出后由 runner 收割。

yield 和阻塞 wait 返回 EL1 runner。轮转选择 ready 进程，安装独立 TTBR0，
以 `boot/resume.S` 恢复完整 GPR/SIMD/FP/PSTATE/SP 并 ERET。Generic Physical
Timer 的实际 EL0 IRQ 也保存完整帧并触发轮转；IRQ 不分配、不输出、不修改
页表。每进程累计 30 个 EL0 timer tick 的诊断预算用于终止失控测试，非
正式 Unix 信号或生产调度策略。内核临时 IRQ mask 不会写入用户 PSTATE。

## 文件与 syscall

真实 SVC dispatcher 通过每进程 FD 表调用同一套 host-tested VFS。支持
open/read/write/close/lseek/fstat/getdents/mkdir/unlink/rename/chdir/getcwd/
dup/dup2。路径为原始字节，支持绝对/相对路径、`.`、`..` 和尾部目录斜杠。
rename 原子替换目标；已打开但被 unlink/替换的文件保留到最后一个引用
关闭。dup、dup2 和 spawn 的继承 FD 共享偏移；独立 open 各有偏移。

fd0 是 Mini UART 轮询输入，暂无数据返回 EAGAIN，不阻塞被屏蔽 IRQ 的
CPU0。fd1/fd2 输出到现有 UART/HDMI 控制台；dup2 可重定向到普通文件。
read/write 每次最多 4096 字节。完整用户范围、访问权限及物理页所有权先
校验，再进行文件偏移/目录游标/输入消费/命名空间修改；无效尾页不会造成
部分输出或文件内容修改。所有用户复制通过 EL1 特权身份别名完成。

容量明确且耗尽可恢复：64 个 inode、32 个 open description、每进程 16 个
FD，16 个 RAM 文件各 4096 字节；每个路径最多 256 字节，单名称 48 字节。
RAM 文件支持稀疏零洞、append、truncate、短写和 EOF，重用文件块先清零。
目录删除要求为空且不作为 cwd；`/init` 和 `/tmp` 挂载锚点不可移动。
这不是磁盘存储；重启丢失 RAM 内容。pipes、TTY 行规程、权限凭据、持久
块设备与可写磁盘文件系统留到后续，libc/toolchain 未被本阶段实现。

## 独立程序与验收

`userspace/` 中三个 AArch64 程序各自链接为 ELF。带来源哈希的 ELF/USTAR
fixtures 作为输入资源检入，普通 Cargo 构建无需外部 clang。修改源程序后
用 `scripts/build_userspace_windows.ps1 -UpdateFixtures` 重建；默认脚本重建并
比较 fixtures，`-VerifyFixtures` 只验证来源和二进制哈希。

init 检查正常入口栈、argv/envp、初始化数据和 BSS，验证 yield/wait 的
GPR/SIMD/FP/NZCV/SP/PID 保持及 monotonic 时钟结果；执行 RAM 文件往返、
dup 共享偏移、stat、目录/cwd、rename/unlink 和只读归档读取；spawn child，
child 检查传入参数并 exec 新 ELF，后者退出 7；init wait 校验状态后退出 0。
runner 要求精确 spawn=1/exec=1/wait=1、所有 PID 和 FD 已释放，页/堆统计
回到之前值，才能打印 `[ OK ] SYSCALL -> FILESYSTEM M4 SELF-TEST`。

```powershell
.\scripts\check_windows.ps1
.\scripts\build_windows.ps1 -EnableFilesystem
# 手动部署，使用已有 UART bootloader；不会写启动盘。
.\scripts\upload_kernel_uart.ps1 -EnableFilesystem -Monitor
```

新镜像是 `target/kernel8-fs.img`，kernel/bootloader 地址仍为 0x200000/0x80000。
树莓派需观察 M1/M2/M3 诊断均通过、ELF file roundtrip 输出、exec/exit(7)、
wait/init exit(0)、M4 reclaim、HDMI 和后续持续 TIMER TICK。当前证据与
未验收项见 [M4 验证记录](validation/M4.md)。
