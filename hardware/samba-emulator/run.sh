#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Flashes arduino/BOSSA's emulated Arduino SAM-BA bootloader (tests/samba.py) with nanoflash and
# with bossac, checks the flash and the errors (hardware/samba-emulator/emulate.py), then replays
# the traffic nanoflash recorded.
# BOSSA is BSD-3-Clause: it is fetched at a pinned commit into target/ and only built and run as a
# test tool; none of its code is copied into nanoflash.
# Needs git, python3, make and a C++ compiler, on Linux or macOS (the emulator uses a
# pseudo-terminal).
set -eu

# The commit "Merge pull request #6 from arduino/add-tests" of https://github.com/arduino/BOSSA.
BOSSA_COMMIT=c4943bd19006b95b5c3c0ee54a10ec24accc33de

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

# SAMBA_EMULATOR_DIRECTORY overrides where BOSSA, the images and the transcript go.
directory=${SAMBA_EMULATOR_DIRECTORY:-target/samba-emulator}
mkdir -p "$directory"

if [ ! -d "$directory/BOSSA" ]; then
    mkdir "$directory/BOSSA"
    git -C "$directory/BOSSA" init -q
    git -C "$directory/BOSSA" fetch -q --depth 1 https://github.com/arduino/BOSSA "$BOSSA_COMMIT"
    git -C "$directory/BOSSA" checkout -q FETCH_HEAD
fi
head=$(git -C "$directory/BOSSA" rev-parse HEAD)
if [ "$head" != "$BOSSA_COMMIT" ]; then
    echo "$directory/BOSSA is at $head, not at $BOSSA_COMMIT: remove it and run again" >&2
    exit 1
fi

# The applet source is generated from its assembly with arm-none-eabi-as when the assembly looks
# newer, which a fresh checkout may make it; the checked-in source is enough. VERSION is set since
# a shallow fetch has no tag for git describe, and -Wno-error keeps a newer compiler's warnings
# from failing a tool nanoflash does not maintain. Without wxWidgets, make prints
# "wx-config: not found": only the graphical BOSSA needs it.
touch "$directory/BOSSA/src/WordCopyArm.cpp"
make -C "$directory/BOSSA" -j2 bossac VERSION=1.9.1-emulator CXXFLAGS=-Wno-error

cargo build --quiet -p nanoflash-cli
nanoflash=target/debug/nanoflash

rm -f "$directory/nano33ble.transcript"
python3 -I hardware/samba-emulator/emulate.py --bossa "$directory/BOSSA" \
    --nanoflash "$nanoflash" --directory "$directory"

# Recording and replaying must agree on the traffic of the emulated bootloader.
"$nanoflash" flash --board hardware/samba-emulator/nano33ble.toml \
    --replay "$directory/nano33ble.transcript"
echo "samba emulator: flashed with nanoflash and bossac, images identical, transcript replayed"
