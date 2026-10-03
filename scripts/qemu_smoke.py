#!/usr/bin/env python3
"""Bounded Raspberry Pi 4 emulation smoke; never constitutes board validation.

Loads the unmodified kernel ELF with the generic loader. A synthetic DTB and
minimal x0/PC handoff substitute for firmware/serial bootloader deployment.
Only the QEMU subprocess started here is terminated. No removable media,
kernel registers, device addresses or kernel source are adjusted for QEMU.

QEMU 11.1 models Cortex-A72 CPUECTLR_EL1 as constant zero, so the real kernel
correctly stops MMU bring-up at CoherencyDisabled. This harness reports that
emulator limitation after observing continuing timer IRQs; it never bypasses
the SMPEN safeguard. Source: QEMU v11.1.0 target/arm/cortex-regs.c.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import time


def synthetic_dtb() -> bytes:
    strings = bytearray()
    structure = bytearray()

    def word(value: int) -> bytes:
        return struct.pack(">I", value)

    def align(buffer: bytearray) -> None:
        buffer.extend(bytes((-len(buffer)) % 4))

    def begin(name: bytes) -> None:
        structure.extend(word(1) + name + b"\0")
        align(structure)

    def prop(name: bytes, data: bytes) -> None:
        offset = len(strings)
        strings.extend(name + b"\0")
        structure.extend(word(3) + word(len(data)) + word(offset) + data)
        align(structure)

    begin(b"")
    prop(b"#address-cells", word(2))
    prop(b"#size-cells", word(2))
    begin(b"memory@0")
    prop(b"device_type", b"memory\0")
    prop(b"reg", struct.pack(">QQ", 0, 2 * 1024**3))
    structure.extend(word(2) + word(2) + word(9))
    reservation = bytes(16)
    structure_offset = 40 + len(reservation)
    string_offset = structure_offset + len(structure)
    total = string_offset + len(strings)
    header = struct.pack(">10I", 0xD00DFEED, total, structure_offset, string_offset, 40,
                         17, 16, 0, len(strings), len(structure))
    return header + reservation + structure + strings


def startup() -> bytes:
    # movz x0, #0x10, lsl #16 ; movz x1, #0x20, lsl #16 ; br x1
    # DTB 0x100000 and entry 0x200000 follow the repository handoff contract.
    return struct.pack("<III", 0xD2A00200, 0xD2A00401, 0xD61F0020)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kernel", type=Path, required=True, help="Built AArch64 kernel ELF")
    parser.add_argument("--qemu", default="qemu-system-aarch64")
    parser.add_argument("--timeout", type=float, default=45.0)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--probe", action="store_true", help="Read QEMU reset registers only")
    args = parser.parse_args()
    if not 0 < args.timeout <= 60:
        parser.error("timeout must be greater than 0 and at most 60 seconds")
    executable = shutil.which(args.qemu)
    if executable is None:
        parser.error(f"QEMU unavailable: {args.qemu}")
    kernel = args.kernel.resolve(strict=True)
    output = args.output.resolve() if args.output else Path(tempfile.mkdtemp(prefix="ananos-qemu-"))
    output.mkdir(parents=True, exist_ok=True)
    flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    base = [executable, "-M", "raspi4b", "-display", "none", "-serial", "null"]
    if args.probe:
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        process = subprocess.Popen(base + ["-serial", "null", "-monitor", "none", "-qmp",
                                           f"tcp:127.0.0.1:{port},server=on,wait=off", "-S"],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, creationflags=flags)
        try:
            deadline = time.monotonic() + 10
            while True:
                try:
                    connection = socket.create_connection(("127.0.0.1", port), timeout=2)
                    break
                except ConnectionRefusedError:
                    if time.monotonic() >= deadline or process.poll() is not None:
                        raise
                    time.sleep(0.1)
            commands = [
                {"execute": "qmp_capabilities"},
                {"execute": "human-monitor-command", "arguments": {"command-line": "info registers"}},
                {"execute": "quit"},
            ]
            with connection:
                connection.settimeout(5)
                with connection.makefile("rwb") as channel:
                    print(channel.readline().decode().strip())
                    for command in commands:
                        channel.write((json.dumps(command) + "\n").encode())
                        channel.flush()
                        while True:
                            reply = json.loads(channel.readline())
                            if "return" in reply or "error" in reply:
                                print(reply.get("return", reply))
                                break
            stdout, stderr = process.communicate(timeout=5)
            print(stderr.decode(errors="replace"))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        return 0

    source_kernel = kernel
    snapshot = output / "kernel.elf"
    if snapshot.resolve() != kernel:
        shutil.copyfile(kernel, snapshot)
    kernel = snapshot
    dtb = output / "synthetic.dtb"
    trampoline = output / "handoff.bin"
    uart = output / "uart.log"
    stderr_path = output / "qemu-stderr.log"
    dtb.write_bytes(synthetic_dtb())
    trampoline.write_bytes(startup())
    command = base + [
        "-serial", f"file:{uart}", "-monitor", "none",
        # The machine's normal -kernel boot stub supplies a non-secure EL2
        # firmware handoff. Raw loader CPU reset would start in EL3, which the
        # real kernel intentionally rejects. The raw handoff still sets x0.
        "-kernel", str(trampoline), "-dtb", str(dtb),
        "-device", f"loader,file={kernel}",
        "-device", f"loader,file={dtb},addr=0x100000,force-raw=on",
    ]
    started = time.monotonic()
    completed = False
    blocked = False
    with stderr_path.open("wb") as stderr:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=stderr, creationflags=flags)
        try:
            while process.poll() is None and time.monotonic() - started < args.timeout:
                log = uart.read_bytes() if uart.exists() else b""
                if b"[ OK ] SYSCALL -> FILESYSTEM M4 SELF-TEST" in log:
                    completed = True
                    break
                if b"Cpu(CoherencyDisabled)" in log and log.count(b"TIMER TICK:") >= 5:
                    blocked = True
                    break
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
    log = uart.read_text(errors="replace") if uart.exists() else ""
    summary = {
        "machine": "raspi4b",
        "kernel": str(kernel),
        "source_kernel": str(source_kernel),
        "seconds": round(time.monotonic() - started, 2),
        "uart_bytes": len(log.encode()),
        "completed_marker": completed,
        "status": "completed" if completed else "blocked" if blocked else "incomplete",
        "blocked_reason": "QEMU CPUECTLR_EL1 is constant zero; SMPEN unavailable" if blocked else None,
        "observed": {
            "boot": "[ OK ] Boot assembly" in log,
            "mailbox": "[ OK ] Property Mailbox" in log,
            "framebuffer": "[ OK ] Framebuffer allocated" in log,
            "pages": "[ OK ] PHYSICAL PAGE SELF-TEST" in log,
            "heap": "[ OK ] HEAP ALLOCATION/FREE SELF-TEST" in log,
            "mmu": "[ OK ] MMU BRING-UP SELF-TEST" in log,
            "timer_ticks": log.count("TIMER TICK:"),
            "el0": "[ OK ] MMU -> EL0 GATE A SELF-TEST" in log,
            "m4_reclaim": "[ OK ] M4 ADDRESS-SPACE/FD/PAGE RECLAIM" in log,
        },
        "qemu_exit": process.returncode,
        "output": str(output),
        "hardware_validation": False,
        "command": command,
    }
    (output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    markers = ("SELF-TEST", "[FAIL]", "Mini UART", "Property Mailbox", "Framebuffer allocated",
               "Current exception", "M4 PROCESS", "TIMER TICK:", "RECLAIM")
    evidence = [line for line in log.splitlines() if any(marker in line for marker in markers)]
    print("UART evidence:\n" + "\n".join(evidence[-30:]))
    print("QEMU stderr:\n" + stderr_path.read_text(errors="replace")[-2000:])
    return 0 if completed else 2 if blocked else 1


if __name__ == "__main__":
    raise SystemExit(main())
