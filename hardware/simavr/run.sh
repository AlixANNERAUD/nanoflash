#!/bin/sh
# Flashes Arduino's STK500 bootloaders (Optiboot, the old Nano bootloader ATmegaBOOT and the Mega
# 2560's wiring bootloader) emulated by simavr through nanoflash, checks that the flash holds the
# expected image, then replays the recorded traffic.
# Usage: hardware/simavr/run.sh
# Needs Linux, curl, a C compiler and simavr's static libraries: libsimavr-dev (Debian, Ubuntu),
# installed or, without root, fetched with apt-get download. simavr and the bootloaders are GPL:
# they are only downloaded into target/ and run, never committed or distributed.
set -eu

# The commit of https://github.com/arduino/ArduinoCore-avr the bootloader images come from.
ARDUINO_AVR_COMMIT=11b9130371e8447920edb65a75706a6c951e51fc

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

# SIMAVR_DIRECTORY overrides where the images, the harness, the dumps and the transcripts go.
directory=${SIMAVR_DIRECTORY:-target/simavr}
mkdir -p "$directory"

for image in optiboot/optiboot_atmega328.hex atmega/ATmegaBOOT_168_atmega328.hex \
    stk500v2/stk500boot_v2_mega2560.hex; do
    file=$directory/$(basename "$image")
    if [ ! -s "$file" ]; then
        curl --fail --silent --show-error --location --output "$file" \
            "https://raw.githubusercontent.com/arduino/ArduinoCore-avr/$ARDUINO_AVR_COMMIT/bootloaders/$image"
    fi
done

# simavr's headers and static libraries: installed (pkg-config names them; --variable does not
# require the libelf and gl packages its .pc files list as private dependencies), else unpacked
# from the libsimavr-dev package.
includedir=
libdir=
if includedir=$(pkg-config --variable=includedir simavr 2>/dev/null) &&
    libdir=$(pkg-config --variable=libdir simavr 2>/dev/null) &&
    [ -f "$libdir/libsimavr.a" ] && [ -f "$libdir/libsimavrparts.a" ]; then
    :
elif command -v apt-get >/dev/null && command -v dpkg-deb >/dev/null; then
    if [ ! -d "$directory/root" ]; then
        mkdir -p "$directory/debs"
        (cd "$directory/debs" && apt-get download libsimavr-dev)
        for package in "$directory"/debs/*.deb; do
            dpkg-deb -x "$package" "$directory/root"
        done
    fi
    includedir=$directory/root/usr/include
    libdir=$(dirname "$(find "$directory/root" -name libsimavr.a | head -n 1)")
else
    echo "simavr was not found: install libsimavr-dev (Debian, Ubuntu)" >&2
    exit 1
fi

"${CC:-cc}" -O1 -o "$directory/harness" hardware/simavr/harness.c \
    -I"$includedir/simavr" -I"$includedir/simavr/parts" \
    "$libdir/libsimavrparts.a" "$libdir/libsimavr.a" -lpthread -lutil

cargo build --quiet -p nanoflash-cli
nanoflash=target/debug/nanoflash

harness_process=
stop() {
    if [ -n "$harness_process" ]; then
        kill "$harness_process" 2>/dev/null || true
        # A short grace period (the harness exits at once on SIGTERM), then no mercy.
        grace=0
        while kill -0 "$harness_process" 2>/dev/null && [ "$grace" -lt 100 ]; do
            grace=$((grace + 1))
            sleep 0.1
        done
        kill -KILL "$harness_process" 2>/dev/null || true
        wait "$harness_process" 2>/dev/null || true
    fi
    harness_process=
}
trap stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Waits up to $2 tenths of a second for the line $1 in the harness log $3.
wait_for() {
    tenths=0
    until grep -q "$1" "$3"; do
        tenths=$((tenths + 1))
        if [ "$tenths" -gt "$2" ]; then
            return 1
        fi
        sleep 0.1
    done
}

# scenario <name> <mcu> <bootloader image> <board file> <application size> [flash arguments]
scenario() {
    name=$1
    mcu=$2
    bootloader=$directory/$3
    board=hardware/simavr/$4
    size=$5
    shift 5
    log=$directory/$name.log
    rm -f "$directory/$name.dump" "$directory/$name.transcript" "$log"

    "$directory/harness" "$mcu" "$bootloader" "$directory/$name.dump" >"$log" 2>&1 &
    harness_process=$!
    if ! wait_for '^port ' 100 "$log"; then
        echo "$name: the harness did not open its pseudo-terminal:" >&2
        cat "$log" >&2
        exit 1
    fi
    port=$(sed -n 's/^port //p' "$log")

    if ! "$nanoflash" flash --board "$board" --port "$port" "$@" \
        --record "$directory/$name.transcript" --json >"$directory/$name.json"; then
        echo "$name: the flash failed:" >&2
        cat "$directory/$name.json" "$log" >&2
        exit 1
    fi
    if ! wait_for '^application started' 50 "$log"; then
        echo "$name: the bootloader did not start the application:" >&2
        cat "$log" >&2
        exit 1
    fi
    stop

    "$nanoflash" image build --board "$board" --fill --output "$directory/$name-expected.bin" \
        >/dev/null
    cmp -n "$size" "$directory/$name.dump" "$directory/$name-expected.bin"

    # Recording and replaying must agree on the traffic of the real bootloader.
    if ! "$nanoflash" flash --board "$board" --replay "$directory/$name.transcript" "$@" \
        >"$directory/$name-replay.log" 2>&1; then
        echo "$name: the replay failed:" >&2
        cat "$directory/$name-replay.log" >&2
        exit 1
    fi
    echo "$name: flashed, flash content identical, transcript replayed"
}

scenario optiboot atmega328p optiboot_atmega328.hex uno.toml $((0x7E00))
# simavr ignores the host's baud rate: this exercises the explicit rate and the 2 KiB bootloader.
scenario atmegaboot atmega328p ATmegaBOOT_168_atmega328.hex nano.toml $((0x7800)) \
    --baud-rate 57600
scenario mega2560 atmega2560 stk500boot_v2_mega2560.hex mega2560.toml $((0x3E000))
