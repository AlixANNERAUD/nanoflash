#!/bin/sh
# Flashes a chip emulated by Espressif's QEMU in download mode through nanoflash, checks that the
# flash holds the expected image, then replays the recorded traffic.
# Usage: hardware/qemu/run.sh <esp32|esp32c3>
# Needs curl, tar, sha256sum and an x86-64 or AArch64 Linux host with libslirp (Debian:
# libslirp0), which QEMU links against.
set -eu

# The newest release of https://github.com/espressif/qemu/releases; the asset names carry the
# version with underscores.
QEMU_RELEASE=esp-develop-9.2.2-20261109
QEMU_VERSION=esp_develop_9.2.2_20261109

chip=${1:?usage: hardware/qemu/run.sh <esp32|esp32c3>}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

# QEMU_DIRECTORY overrides where QEMU, the flash and the transcripts go.
qemu_directory=${QEMU_DIRECTORY:-target/qemu}
mkdir -p "$qemu_directory"
board=hardware/qemu/$chip.toml

case "$chip" in
esp32) architecture=xtensa ;;
esp32c3) architecture=riscv32 ;;
*)
    echo "unknown chip '$chip': use esp32 or esp32c3" >&2
    exit 1
    ;;
esac

host=$(uname -m)
case "$host" in
x86_64 | aarch64) ;;
arm64) host=aarch64 ;;
*)
    echo "no QEMU build for a $host host: use x86-64 or AArch64 Linux" >&2
    exit 1
    ;;
esac

# The SHA-256 digests of the release assets, as published in its checksum file.
case "$architecture-$host" in
xtensa-x86_64) digest=fbc3bffa9fa71192847969a627a584aabe2e86d85bed4902a096ff06c2b8a815 ;;
xtensa-aarch64) digest=dad4a8f44b5649b1f110963d479a77edb0ad2484cde79403e3a19c50efdd5281 ;;
riscv32-x86_64) digest=afa59465495295b8aa4f804768983fa30b0bbf33d0f68a047f9caa436b9164c0 ;;
riscv32-aarch64) digest=4038e03d6e312b2372106516855a2c72eda02b49f6480917c55e56e04d958781 ;;
esac

# Both archives unpack into target/qemu/qemu/.
binary=$qemu_directory/qemu/bin/qemu-system-$architecture
if [ ! -x "$binary" ]; then
    asset=qemu-$architecture-softmmu-$QEMU_VERSION-$host-linux-gnu.tar.xz
    curl --fail --silent --show-error --location \
        --output "$qemu_directory/$asset" \
        "https://github.com/espressif/qemu/releases/download/$QEMU_RELEASE/$asset"
    if ! echo "$digest  $qemu_directory/$asset" | sha256sum --check --status; then
        echo "$asset does not have the SHA-256 digest $digest" >&2
        rm -f "$qemu_directory/$asset"
        exit 1
    fi
    tar -xJf "$qemu_directory/$asset" -C "$qemu_directory"
    rm -f "$qemu_directory/$asset"
fi

cargo build --quiet -p nanoflash-cli
nanoflash=target/debug/nanoflash

# A blank 4 MiB flash.
head -c 4194304 /dev/zero | tr '\000' '\377' >"$qemu_directory/flash.bin"
rm -f "$qemu_directory/$chip.transcript" "$qemu_directory/qemu.log"

qemu_process=
stop() {
    if [ -n "$qemu_process" ]; then
        kill "$qemu_process" 2>/dev/null || true
        # A short grace period, then no mercy.
        grace=0
        while kill -0 "$qemu_process" 2>/dev/null && [ "$grace" -lt 10 ]; do
            grace=$((grace + 1))
            sleep 1
        done
        kill -KILL "$qemu_process" 2>/dev/null || true
        wait "$qemu_process" 2>/dev/null || true
    fi
    qemu_process=
}
trap stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Flags of the esp32 and esp32c3 pages of https://github.com/espressif/esp-toolchain-docs/tree/main/qemu
# (strap modes: 0x0f UART download on the ESP32, 0x02 UART0 download on the ESP32-C3). The serial
# port is a pseudo-terminal, whose path QEMU prints.
case "$chip" in
esp32)
    "$binary" -nographic -monitor none -machine esp32 \
        -drive "file=$qemu_directory/flash.bin,if=mtd,format=raw" \
        -global driver=esp32.gpio,property=strap_mode,value=0x0f \
        -serial pty </dev/null >"$qemu_directory/qemu.log" 2>&1 &
    # The emulated ESP32 stops answering after CHANGE_BAUDRATE; the ESP32-C3 does not.
    baud_rate=115200
    ;;
esp32c3)
    head -c 1024 /dev/zero >"$qemu_directory/efuse.bin"
    "$binary" -nographic -monitor none -icount 3 -machine esp32c3 \
        -drive "file=$qemu_directory/flash.bin,if=mtd,format=raw" \
        -drive "file=$qemu_directory/efuse.bin,if=none,format=raw,id=efuse" \
        -global driver=nvram.esp32c3.efuse,property=drive,value=efuse \
        -global driver=esp32c3.gpio,property=strap_mode,value=0x02 \
        -serial pty </dev/null >"$qemu_directory/qemu.log" 2>&1 &
    baud_rate=460800
    ;;
esac
qemu_process=$!

port=
waited=0
while [ -z "$port" ]; do
    port=$(sed -n 's|.*redirected to \(/dev/pts/[0-9]*\).*|\1|p' "$qemu_directory/qemu.log")
    waited=$((waited + 1))
    if [ -z "$port" ] && [ "$waited" -gt 60 ]; then
        echo "QEMU did not open its serial port:" >&2
        cat "$qemu_directory/qemu.log" >&2
        exit 1
    fi
    [ -n "$port" ] || sleep 1
done

"$nanoflash" flash --board "$board" --port "$port" --baud-rate "$baud_rate" \
    --record "$qemu_directory/$chip.transcript" --json

stop

"$nanoflash" image build --board "$board" --fill --output "$qemu_directory/expected.bin"
cmp "$qemu_directory/flash.bin" "$qemu_directory/expected.bin"

# Recording and replaying must agree on the traffic of the real ROM.
"$nanoflash" flash --board "$board" --replay "$qemu_directory/$chip.transcript" \
    --baud-rate "$baud_rate"
echo "$chip: flashed, flash content identical, transcript replayed"
