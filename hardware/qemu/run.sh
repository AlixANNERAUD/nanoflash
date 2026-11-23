#!/bin/sh
# Flashes a chip emulated by Espressif's QEMU in download mode through nanoflash, checks that the
# flash holds the expected image, then replays the recorded traffic.
# Usage: hardware/qemu/run.sh <esp32|esp32c3>   (needs socat, curl, tar and an x86-64 Linux host)
set -eu

# The newest release of https://github.com/espressif/qemu/releases; the asset names carry the
# version with underscores.
QEMU_RELEASE=esp-develop-9.2.2-20261109
QEMU_VERSION=esp_develop_9.2.2_20261109

chip=${1:?usage: hardware/qemu/run.sh <esp32|esp32c3>}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

qemu_directory=target/qemu
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

# Both archives unpack into target/qemu/qemu/.
binary=$qemu_directory/qemu/bin/qemu-system-$architecture
if [ ! -x "$binary" ]; then
    asset=qemu-$architecture-softmmu-$QEMU_VERSION-x86_64-linux-gnu.tar.xz
    curl --fail --silent --show-error --location \
        --output "$qemu_directory/$asset" \
        "https://github.com/espressif/qemu/releases/download/$QEMU_RELEASE/$asset"
    tar -xJf "$qemu_directory/$asset" -C "$qemu_directory"
fi

cargo build --quiet -p nanoflash-cli
nanoflash=target/debug/nanoflash

# A blank 4 MiB flash.
head -c 4194304 /dev/zero | tr '\000' '\377' >"$qemu_directory/flash.bin"
rm -f "$qemu_directory/serial" "$qemu_directory/$chip.transcript"

qemu_process=
socat_process=
stop() {
    for process in $socat_process $qemu_process; do
        kill "$process" 2>/dev/null || true
        # A short grace period, then no mercy.
        grace=0
        while kill -0 "$process" 2>/dev/null && [ "$grace" -lt 10 ]; do
            grace=$((grace + 1))
            sleep 1
        done
        kill -KILL "$process" 2>/dev/null || true
        wait "$process" 2>/dev/null || true
    done
    qemu_process=
    socat_process=
}
trap stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Flags of the esp32 and esp32c3 pages of https://github.com/espressif/esp-toolchain-docs/tree/main/qemu
# (strap modes: 0x0f UART download on the ESP32, 0x02 UART0 download on the ESP32-C3).
case "$chip" in
esp32)
    "$binary" -nographic -machine esp32 \
        -drive "file=$qemu_directory/flash.bin,if=mtd,format=raw" \
        -global driver=esp32.gpio,property=strap_mode,value=0x0f \
        -serial tcp:127.0.0.1:5555,server,nowait </dev/null &
    ;;
esp32c3)
    head -c 1024 /dev/zero >"$qemu_directory/efuse.bin"
    "$binary" -nographic -icount 3 -machine esp32c3 \
        -drive "file=$qemu_directory/flash.bin,if=mtd,format=raw" \
        -drive "file=$qemu_directory/efuse.bin,if=none,format=raw,id=efuse" \
        -global driver=nvram.esp32c3.efuse,property=drive,value=efuse \
        -global driver=esp32c3.gpio,property=strap_mode,value=0x02 \
        -serial tcp:127.0.0.1:5555,server,nowait </dev/null &
    ;;
esac
qemu_process=$!

# The serial port of the emulated chip as a pseudo-terminal; socat retries until QEMU listens.
socat "pty,raw,echo=0,link=$qemu_directory/serial" tcp:127.0.0.1:5555,retry=30,interval=1 &
socat_process=$!

waited=0
while [ ! -e "$qemu_directory/serial" ]; do
    waited=$((waited + 1))
    if [ "$waited" -gt 60 ]; then
        echo "the serial port $qemu_directory/serial did not appear" >&2
        exit 1
    fi
    sleep 1
done

"$nanoflash" flash --board "$board" --port "$qemu_directory/serial" \
    --record "$qemu_directory/$chip.transcript" --json

stop

"$nanoflash" image build --board "$board" --fill --output "$qemu_directory/expected.bin"
cmp "$qemu_directory/flash.bin" "$qemu_directory/expected.bin"

# Recording and replaying must agree on the traffic of the real ROM.
"$nanoflash" flash --board "$board" --replay "$qemu_directory/$chip.transcript"
echo "$chip: flashed, flash content identical, transcript replayed"
