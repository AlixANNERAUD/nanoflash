# SPDX-License-Identifier: MIT OR Apache-2.0
"""Flash arduino/BOSSA's emulated SAM-BA bootloader with nanoflash and with bossac.

Usage: emulate.py --bossa DIRECTORY --nanoflash BINARY --directory DIRECTORY

Each scenario starts a fresh emulated Arduino Nano 33 BLE bootloader on a pseudo-terminal
(`SambaDevice` of BOSSA's tests/samba.py, which this script imports and runs, never copies),
runs one command against it, then checks the exit code, the JSON report, the emulated flash and
the events the emulator saw. Prints `<name> ok` or `<name> FAILED: <why>` per scenario and exits
with 1 if any failed.
"""

import argparse
import json
import select
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BOARD = HERE / "nano33ble.toml"
APPLICATION = ROOT / "crates/nanoflash/tests/images/application.bin"

# The Nano 33 BLE as the emulator models it: commands address the flash from 0x10000, the
# application start; a copy erases the 4 KiB pages it touches.
DEVICE = {"base": 0x10000, "flash_size": 0x100000, "erase_block": 4096}
APPLICATION_START = 0x10000
PAGE = 4096
TIMEOUT = 120

# The board of nano33ble.toml without its layout: `{image}` is placed by `offset`.
MANIFEST = """[[board]]
identifier = "arduino-nano-33-ble-emulated"
protocol = "samba"
chip = "nrf52840"
reset = "none"

[[board.image]]
offset = {offset:#x}
path = '{path}'
"""


class Failure(Exception):
    pass


def check(condition, why):
    if not condition:
        raise Failure(why)


def run(arguments):
    """Run a command; returns its exit code and its standard output and error as text."""
    try:
        completed = subprocess.run(
            arguments,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            timeout=TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        raise Failure("%s did not finish within %d s" % (arguments[0], TIMEOUT))
    return completed.returncode, completed.stdout, completed.stderr


class Emulator:
    """One emulated bootloader, stopped when the scenario ends."""

    def __init__(self, samba, **options):
        self.device = samba.SambaDevice(**dict(DEVICE, **options))

    def __enter__(self):
        self.device.attach()
        return self.device

    def __exit__(self, *exception):
        # The emulator stops without reading what is pending, such as the K# a command wrote just
        # before it exited: wait until it has read everything, for 2 s at most.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline and select.select([self.device.master], [], [], 0)[0]:
            time.sleep(0.01)
        self.device.stop()


class Context:
    def __init__(self, samba, nanoflash, bossac, directory):
        self.samba = samba
        self.nanoflash = nanoflash
        self.bossac = bossac
        self.directory = directory

    def emulator(self, **options):
        return Emulator(self.samba, **options)

    def flash(self, device, board, *extra):
        """Flash `board` with nanoflash on `device`; returns the exit code and the document."""
        code, output, errors = run(
            [self.nanoflash, "flash", "--board", str(board), "--port", device.port, "--json"]
            + list(extra)
        )
        try:
            document = json.loads(output)
        except ValueError:
            raise Failure(
                "nanoflash exited with %d without a JSON document: %s%s" % (code, output, errors)
            )
        return code, document

    def manifest(self, name, offset, path):
        manifest = self.directory / name
        manifest.write_text(MANIFEST.format(offset=offset, path=path))
        return manifest


def exits(code, expected, document):
    check(
        code == expected,
        "exit code %d, expected %d: %s" % (code, expected, json.dumps(document)),
    )


def error_kind(document):
    errors = document.get("errors") or [{}]
    return errors[0].get("kind")


def blank(memory, start, end):
    return all(byte == 0xFF for byte in memory[start:end])


def first_difference(left, right):
    for index, (a, b) in enumerate(zip(left, right)):
        if a != b:
            return "0x%x: 0x%02x against 0x%02x" % (index, a, b)
    return "different lengths"


def nanoflash(context):
    application = APPLICATION.read_bytes()
    end = APPLICATION_START + len(application)
    with context.emulator() as device:
        code, document = context.flash(
            device, BOARD, "--record", str(context.directory / "nano33ble.transcript")
        )
    exits(code, 0, document)
    check((document.get("report") or {}).get("verified") is True, "the report is not verified")
    memory = device.memory
    check(memory[APPLICATION_START:end] == application, "the flash does not hold application.bin")
    check(
        blank(memory, 0, APPLICATION_START) and blank(memory, end, len(memory)),
        "the flash outside application.bin is not 0xFF",
    )
    events = device.events
    check(events[:1] == ["chip-erase:0x0"], "the first event is not chip-erase:0x0: %s" % events)
    check(events[-1:] == ["reset"], "the last event is not reset: %s" % events)


def same_flash_as_bossac(context):
    # Not a multiple of the 4 KiB page, with a different byte pattern in every page.
    image = bytes((i * 7 + i // 4096) % 256 for i in range(154600))
    sketch = context.directory / "sketch.bin"
    sketch.write_bytes(image)
    manifest = context.manifest("sketch.toml", APPLICATION_START, "sketch.bin")
    with context.emulator() as device:
        code, document = context.flash(device, manifest)
    exits(code, 0, document)
    ours = bytes(device.memory)
    with context.emulator() as device:
        # The flags of nanoglia's bossac command line, without -d.
        code, output, errors = run(
            [
                context.bossac,
                "--port=%s" % device.port,
                "-U",
                "-i",
                "-e",
                "-w",
                "-v",
                str(sketch),
                "-R",
            ]
        )
    check(code == 0, "bossac exited with %d: %s%s" % (code, output, errors))
    theirs = bytes(device.memory)
    end = APPLICATION_START + len(image)
    page_end = -(-end // PAGE) * PAGE
    check(ours[APPLICATION_START:end] == image, "nanoflash did not write the image")
    check(
        ours[APPLICATION_START:end] == theirs[APPLICATION_START:end],
        "the images differ at %s"
        % first_difference(ours[APPLICATION_START:end], theirs[APPLICATION_START:end]),
    )
    # The last page is padded: with 0xFF by nanoflash, with 0x00 by bossac.
    check(blank(ours, end, page_end), "nanoflash did not pad the last page with 0xFF")
    check(
        all(byte == 0 for byte in theirs[end:page_end]),
        "bossac did not pad the last page with 0x00",
    )
    check(
        ours[:APPLICATION_START] == theirs[:APPLICATION_START]
        and ours[page_end:] == theirs[page_end:],
        "the flash outside the image differs",
    )


def crc_mismatch(context):
    with context.emulator(corrupt_at=APPLICATION_START + 5000) as device:
        code, document = context.flash(device, BOARD)
    exits(code, 3, document)
    check(
        error_kind(document) == "verification-failed",
        "the error is %s, not verification-failed" % error_kind(document),
    )
    check(device.events[-1:] == ["reset"], "the last event is not reset: %s" % device.events)


def no_bootloader(context):
    with context.emulator(silent=True) as device:
        code, document = context.flash(device, BOARD)
    exits(code, 2, document)
    check(
        error_kind(document) == "no-bootloader",
        "the error is %s, not no-bootloader" % error_kind(document),
    )
    check(
        not any(event.startswith("write:") for event in device.events),
        "the emulator saw a write: %s" % device.events,
    )


def below_the_application(context):
    manifest = context.manifest("below.toml", 0x0, APPLICATION)
    with context.emulator() as device:
        code, document = context.flash(device, manifest)
    exits(code, 1, document)
    check(
        error_kind(document) == "below-writable-flash",
        "the error is %s, not below-writable-flash" % error_kind(document),
    )
    check(
        not any(event.startswith(("chip-erase", "write:")) for event in device.events),
        "the emulator saw an erase or a write: %s" % device.events,
    )


SCENARIOS = [
    ("nanoflash", nanoflash),
    ("same-flash-as-bossac", same_flash_as_bossac),
    ("crc-mismatch", crc_mismatch),
    ("no-bootloader", no_bootloader),
    ("below-the-application", below_the_application),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bossa", required=True, type=Path, help="the BOSSA checkout")
    parser.add_argument("--nanoflash", required=True, type=Path, help="the nanoflash binary")
    parser.add_argument("--directory", required=True, type=Path, help="where files are written")
    arguments = parser.parse_args()

    sys.path.insert(0, str((arguments.bossa / "tests").resolve()))
    import samba

    arguments.directory.mkdir(parents=True, exist_ok=True)
    context = Context(
        samba,
        str(arguments.nanoflash.resolve()),
        str((arguments.bossa / "bin/bossac").resolve()),
        arguments.directory.resolve(),
    )
    failed = False
    for name, scenario in SCENARIOS:
        try:
            scenario(context)
        except Failure as failure:
            print("%s FAILED: %s" % (name, failure), flush=True)
            failed = True
        else:
            print("%s ok" % name, flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
