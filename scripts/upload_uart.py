import argparse
import struct
import sys
import time

try:
    import serial
except ImportError:
    print("pyserial is required: python -m pip install pyserial", file=sys.stderr)
    raise


MAGIC = b"ANAN"


def main() -> int:
    parser = argparse.ArgumentParser(description="Upload AnanOS kernel8.img over UART.")
    parser.add_argument("image", help="Path to kernel8.img")
    parser.add_argument("--port", default="COM5", help="Serial port, for example COM5")
    parser.add_argument("--baud", type=int, default=115200, help="UART baud rate")
    parser.add_argument("--timeout", type=float, default=30.0, help="Serial timeout in seconds")
    parser.add_argument(
        "--monitor",
        action="store_true",
        help="Keep the serial port open after upload and print kernel output.",
    )
    args = parser.parse_args()

    with open(args.image, "rb") as image_file:
        data = image_file.read()

    checksum = sum(data) & 0xFFFFFFFF

    print(f"Opening {args.port} at {args.baud} baud")
    with serial.Serial(args.port, args.baud, timeout=args.timeout, write_timeout=args.timeout) as uart:
        saw_ready = wait_for_ready(uart, args.timeout)
        if not saw_ready:
            print("Tip: reset or power-cycle the Raspberry Pi while this script is waiting.")

        print(f"Uploading {len(data)} bytes")
        uart.write(MAGIC)
        uart.write(struct.pack("<I", len(data)))

        sent = 0
        chunk_size = 1024
        last_report = time.monotonic()
        while sent < len(data):
            chunk = data[sent : sent + chunk_size]
            uart.write(chunk)
            sent += len(chunk)

            now = time.monotonic()
            if now - last_report >= 0.25 or sent == len(data):
                percent = sent * 100 // len(data)
                print(f"\r{sent}/{len(data)} bytes ({percent}%)", end="", flush=True)
                last_report = now

        print()
        uart.write(struct.pack("<I", checksum))
        uart.flush()

        response = read_response(uart, args.timeout)
        if response:
            print(response)
        else:
            print("No response from bootloader after upload.", file=sys.stderr)
            print("Check that the USB TTL TX/RX wires are crossed and the Pi booted from F:.", file=sys.stderr)

        if not response.startswith("OK"):
            return 1

        if args.monitor:
            monitor(uart)

    return 0


def wait_for_ready(uart: serial.Serial, timeout: float) -> bool:
    print("Waiting for bootloader READY line.")
    deadline = time.monotonic() + timeout
    while True:
        if time.monotonic() >= deadline:
            print("READY not seen; sending anyway.")
            return False

        line = uart.readline().decode(errors="replace").strip()
        if line:
            print(line)
        if "READY" in line:
            return True


def read_response(uart: serial.Serial, timeout: float) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = uart.readline().decode(errors="replace").strip()
        if line:
            return line
    return ""


def monitor(uart: serial.Serial) -> None:
    print("Monitoring serial output. Press Ctrl+C to exit.")
    uart.timeout = 0.1
    try:
        while True:
            data = uart.read(4096)
            if data:
                print(data.decode(errors="replace"), end="", flush=True)
    except KeyboardInterrupt:
        print("\nMonitor stopped.")


if __name__ == "__main__":
    raise SystemExit(main())
