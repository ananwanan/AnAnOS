param(
    # Arm GNU Toolchain 的 bin 目录。
    # 已加入 PATH 时可以留空。
    [string]$ArmToolchainBin = "",

    [int]$GdbPort = 10120,

    # 不启动 GDB，只启动 QEMU。
    [switch]$QemuOnly,

    # 跳过编译，直接使用现有内核。
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# ------------------------------------------------------------
# 路径
# ------------------------------------------------------------

$ScriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDirectory

Set-Location $ProjectRoot

$KernelElf = Join-Path `
    $ProjectRoot `
    "target\aarch64-unknown-none\debug\kernel"

$KernelImage = Join-Path $ProjectRoot "target/kernel8.img"
$GdbCommandFile = Join-Path $ProjectRoot "target/.gdb-qemu"

# ------------------------------------------------------------
# 输出工具
# ------------------------------------------------------------

function Write-Step {
    param([string]$Message)

    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Success {
    param([string]$Message)

    Write-Host "[ OK ] $Message" -ForegroundColor Green
}

function Write-Failure {
    param([string]$Message)

    Write-Host "[FAIL] $Message" -ForegroundColor Red
}

# ------------------------------------------------------------
# 查找程序
# ------------------------------------------------------------

function Resolve-Executable {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [string[]]$AdditionalPaths = @()
    )

    $command = Get-Command $Name `
        -CommandType Application `
        -ErrorAction SilentlyContinue

    if ($null -ne $command) {
        return $command.Source
    }

    foreach ($path in $AdditionalPaths) {
        if ([string]::IsNullOrWhiteSpace($path)) {
            continue
        }

        $candidate = Join-Path $path $Name

        if (Test-Path $candidate -PathType Leaf) {
            return (Resolve-Path $candidate).Path
        }
    }

    return $null
}

# ------------------------------------------------------------
# 检查 TCP 端口
# ------------------------------------------------------------

function Test-TcpPort {
    param(
        [string]$HostName,
        [int]$Port
    )

    $client = [System.Net.Sockets.TcpClient]::new()

    try {
        $task = $client.ConnectAsync($HostName, $Port)

        if (-not $task.Wait(300)) {
            return $false
        }

        return $client.Connected
    }
    catch {
        return $false
    }
    finally {
        $client.Dispose()
    }
}

function Wait-GdbServer {
    param(
        [int]$Port,
        [int]$Attempts = 50
    )

    for ($index = 0; $index -lt $Attempts; $index++) {
        if (Test-TcpPort -HostName "127.0.0.1" -Port $Port) {
            return
        }

        Start-Sleep -Milliseconds 200
    }

    throw "QEMU GDB 服务未在端口 $Port 上启动。"
}

# ------------------------------------------------------------
# 检查依赖
# ------------------------------------------------------------

Write-Step "检查开发工具"

$Cargo = Resolve-Executable -Name "cargo.exe"
$RustObjcopy = Resolve-Executable -Name "rust-objcopy.exe"
$Qemu = Resolve-Executable -Name "qemu-system-aarch64.exe"

$Gdb = Resolve-Executable `
    -Name "aarch64-none-elf-gdb.exe" `
    -AdditionalPaths @($ArmToolchainBin)

if ($null -eq $Cargo) {
    throw "找不到 cargo.exe，请检查 Rust 是否已安装并加入 PATH。"
}

if ($null -eq $RustObjcopy) {
    throw @"
找不到 rust-objcopy.exe。

请执行：

    cargo install cargo-binutils
    rustup component add llvm-tools
"@
}

if ($null -eq $Qemu) {
    throw "找不到 qemu-system-aarch64.exe，请安装 QEMU 并加入 PATH。"
}

if (-not $QemuOnly -and $null -eq $Gdb) {
    throw @"
找不到 aarch64-none-elf-gdb.exe。

请修改脚本开头的 ArmToolchainBin，例如：

    `$ArmToolchainBin = "E:\SDK\arm-gnu-toolchain\bin"

或者运行：

    .\scripts\debug.ps1 -ArmToolchainBin "你的工具链\bin"
"@
}

Write-Success "Cargo: $Cargo"
Write-Success "rust-objcopy: $RustObjcopy"
Write-Success "QEMU: $Qemu"

if ($null -ne $Gdb) {
    Write-Success "GDB: $Gdb"
}

# ------------------------------------------------------------
# 检查 QEMU 是否支持 raspi4b
# ------------------------------------------------------------

Write-Step "检查 QEMU 的 Raspberry Pi 4B 支持"

$MachineList = & $Qemu -machine help 2>&1 | Out-String

if ($MachineList -notmatch "raspi4b") {
    throw @"
当前 QEMU 不支持 raspi4b。

请检查：

    qemu-system-aarch64 -machine help

建议安装包含 Raspberry Pi 4B 模拟支持的新版本 QEMU。
"@
}

Write-Success "QEMU 支持 raspi4b"

# ------------------------------------------------------------
# 编译
# ------------------------------------------------------------

if (-not $NoBuild) {
    Write-Step "编译 Rust 内核"

    & $Cargo build -p kernel

    if ($LASTEXITCODE -ne 0) {
        throw "Cargo 编译失败，退出代码：$LASTEXITCODE"
    }

    Write-Success "内核 ELF 编译完成"

    if (-not (Test-Path $KernelElf -PathType Leaf)) {
        throw "没有找到内核 ELF：$KernelElf"
    }

    Write-Step "生成 kernel8.img"

    & $RustObjcopy `
        --strip-all `
        -O binary `
        $KernelElf `
        $KernelImage

    if ($LASTEXITCODE -ne 0) {
        throw "rust-objcopy 执行失败，退出代码：$LASTEXITCODE"
    }

    Write-Success "已生成：$KernelImage"
}
else {
    Write-Step "跳过编译"

    if (-not (Test-Path $KernelElf -PathType Leaf)) {
        throw "没有找到现有内核 ELF：$KernelElf"
    }

    if (-not (Test-Path $KernelImage -PathType Leaf)) {
        throw "没有找到现有镜像：$KernelImage"
    }
}

# ------------------------------------------------------------
# 检查端口是否被占用
# ------------------------------------------------------------

Write-Step "检查 GDB 端口"

if (Test-TcpPort -HostName "127.0.0.1" -Port $GdbPort) {
    throw @"
端口 $GdbPort 已经被占用。

可能已有一个 QEMU 实例正在运行。
请先关闭旧的 QEMU 窗口，或者指定其他端口：

    .\scripts\debug.ps1 -GdbPort 1235
"@
}

Write-Success "端口 $GdbPort 可用"

# ------------------------------------------------------------
# 启动 QEMU
# ------------------------------------------------------------

Write-Step "启动 QEMU"

$QemuArguments = @(
    "-M", "raspi4b",
    "-kernel", "`"$KernelImage`"",
    "-display", "none",
    "-serial", "null",
    "-serial", "stdio",
    "-S",
    "-gdb", "tcp:127.0.0.1:$GdbPort"
)

$QemuCommand = @"
& '$Qemu' $($QemuArguments -join ' ')

Write-Host ''
Write-Host 'QEMU 已退出。' -ForegroundColor Yellow
"@

$QemuProcess = Start-Process `
    -FilePath "powershell.exe" `
    -ArgumentList @(
        "-NoExit",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        $QemuCommand
    ) `
    -WorkingDirectory $ProjectRoot `
    -PassThru

Write-Success "QEMU 进程已启动，PID：$($QemuProcess.Id)"

if ($QemuOnly) {
    Write-Host ""
    Write-Host "QEMU 正在等待 GDB 连接：" -ForegroundColor Yellow
    Write-Host "127.0.0.1:$GdbPort"
    exit 0
}

# ------------------------------------------------------------
# 等待 QEMU GDB Stub
# ------------------------------------------------------------

Write-Step "等待 QEMU GDB 服务"

try {
    Wait-GdbServer -Port $GdbPort
}
catch {
    if (-not $QemuProcess.HasExited) {
        Stop-Process -Id $QemuProcess.Id -Force
    }

    throw
}

Write-Success "GDB 服务已监听 127.0.0.1:$GdbPort"

# ------------------------------------------------------------
# 生成 GDB 命令文件
# ------------------------------------------------------------

Write-Step "生成 GDB 初始化命令"

$GdbCommands = @"
set architecture aarch64
set pagination off
set print pretty on
set confirm off
set disassemble-next-line on

target remote 127.0.0.1:$GdbPort

hbreak _start
hbreak kernel_main

echo \n
echo QEMU + GDB connected.\n
echo Breakpoints: _start, kernel_main\n
echo Use 'continue' or 'c' to start the kernel.\n
echo \n
"@

Set-Content `
    -Path $GdbCommandFile `
    -Value $GdbCommands `
    -Encoding ASCII

Write-Success "GDB 命令文件：$GdbCommandFile"

# ------------------------------------------------------------
# 启动 GDB
# ------------------------------------------------------------

Write-Step "启动 GDB"

Write-Host ""
Write-Host "常用命令：" -ForegroundColor Yellow
Write-Host "  c               继续运行"
Write-Host "  si              单条汇编指令"
Write-Host "  ni              单条汇编指令，不进入调用"
Write-Host "  s               Rust 源码单步"
Write-Host "  n               Rust 源码下一行"
Write-Host "  info registers  查看寄存器"
Write-Host "  bt              查看调用栈"
Write-Host "  x/10i `$pc       查看当前位置的汇编"
Write-Host "  Ctrl+C          暂停卡住的内核"
Write-Host ""

& $Gdb `
    -x $GdbCommandFile `
    $KernelElf

$GdbExitCode = $LASTEXITCODE

Write-Host ""
Write-Host "GDB 已退出，退出代码：$GdbExitCode" -ForegroundColor Yellow

if (-not $QemuProcess.HasExited) {
    $answer = Read-Host "是否关闭 QEMU？[Y/n]"

    if ([string]::IsNullOrWhiteSpace($answer) -or $answer -match "^[Yy]") {
        Stop-Process -Id $QemuProcess.Id -Force
        Write-Success "QEMU 已关闭"
    }
}