// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Runs an Arduino bootloader in simavr with UART0 on a pseudo-terminal, for hardware/simavr/run.sh.
// Usage: harness <mcu> <bootloader.hex> <dump file>
//
// Prints `port <path>` once the pseudo-terminal is open, holds the reset button while the
// application flash is blank, and when the program counter first runs below the bootloader with a
// programmed flash writes the whole flash to the dump file, prints `application started` and
// stops simulating. SIGTERM writes the dump (if not written yet) and exits 0.
//
// simavr is GPL-3.0-or-later: this file is written from its public headers only and calls its
// exported functions. The binary links simavr statically, is built locally into target/ and is
// never distributed.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include <avr_uart.h>
#include <sim_avr.h>
#include <sim_hex.h>
#include <sim_io.h>
#include <sim_regbit.h>
#include <sim_time.h>
#include <uart_pty.h>

// Exported by simavr's uart_pty.c but not declared in its header.
avr_cycle_count_t uart_pty_flush_timer(avr_t *avr, avr_cycle_count_t when, void *param);

#define FREQUENCY 16000000
#define THROTTLE_CYCLES 16000

static avr_t *avr;
static uart_pty_t pty;
static uint32_t boot_start;
static const char *dump_path;
static int dumped;
static volatile sig_atomic_t terminated;

static void on_terminate(int signal_number) {
    (void)signal_number;
    terminated = 1;
}

static int application_blank(void) {
    for (uint32_t address = 0; address < boot_start; address++)
        if (avr->flash[address] != 0xFF)
            return 0;
    return 1;
}

static void dump(void) {
    FILE *file = fopen(dump_path, "wb");
    if (file == NULL || fwrite(avr->flash, 1, avr->flashend + 1, file) != avr->flashend + 1 ||
        fclose(file) != 0) {
        perror(dump_path);
        exit(1);
    }
    dumped = 1;
}

// Moves the bytes the host sent into the UART every millisecond while it accepts them.
static avr_cycle_count_t flush(avr_t *core, avr_cycle_count_t when, void *param) {
    uart_pty_flush_timer(core, when, param);
    return pty.xon ? when + avr_usec_to_cycles(core, 1000) : 0;
}

static void on_xon(avr_irq_t *irq, uint32_t value, void *param) {
    (void)irq, (void)value, (void)param;
    int was_on = pty.xon;
    pty.xon = 1;
    // At once: a timer alone leaves the bootloader waiting for bytes already received.
    uart_pty_flush_timer(avr, avr->cycle, &pty);
    if (!was_on)
        avr_cycle_timer_register_usec(avr, 1000, flush, &pty);
}

static void on_xoff(avr_irq_t *irq, uint32_t value, void *param) {
    (void)irq, (void)value, (void)param;
    pty.xon = 0;
    avr_cycle_timer_cancel(avr, flush, &pty);
}

static double now(void) {
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return time.tv_sec + time.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: harness <mcu> <bootloader.hex> <dump file>\n");
        return 2;
    }
    dump_path = argv[3];
    ihex_chunk_p chunks;
    int count = read_ihex_chunks(argv[2], &chunks);
    if (count <= 0) {
        fprintf(stderr, "cannot read %s\n", argv[2]);
        return 1;
    }
    avr = avr_make_mcu_by_name(argv[1]);
    if (avr == NULL) {
        fprintf(stderr, "simavr does not know the mcu %s\n", argv[1]);
        return 1;
    }
    avr_init(avr);
    avr->frequency = FREQUENCY;
    avr->log = LOG_ERROR;
    memset(avr->flash, 0xFF, avr->flashend + 1);
    boot_start = avr->flashend + 1;
    for (int index = 0; index < count; index++) {
        if (chunks[index].baseaddr + chunks[index].size > avr->flashend + 1) {
            fprintf(stderr, "%s does not fit the flash of the %s\n", argv[2], argv[1]);
            return 1;
        }
        memcpy(avr->flash + chunks[index].baseaddr, chunks[index].data, chunks[index].size);
        if (chunks[index].baseaddr < boot_start)
            boot_start = chunks[index].baseaddr;
    }
    free_ihex_chunks(chunks);
    avr->pc = avr->reset_pc = boot_start;
    avr->codeend = avr->flashend;

    // UART0 on a pseudo-terminal, without the /tmp/simavr-uart0 link uart_pty_connect makes.
    // No copy of the traffic on the console, and no host sleep on a read of an empty UART: that
    // sleep slows simulated time while the wiring bootloader polls, and its every message then
    // took about 2 s (the throttle below keeps time anyway).
    uart_pty_init(avr, &pty);
    uint32_t flags = 0;
    avr_ioctl(avr, AVR_IOCTL_UART_GET_FLAGS('0'), &flags);
    flags &= ~(AVR_UART_FLAG_STDIO | AVR_UART_FLAG_POLL_SLEEP);
    avr_ioctl(avr, AVR_IOCTL_UART_SET_FLAGS('0'), &flags);
    avr_connect_irq(avr_io_getirq(avr, AVR_IOCTL_UART_GETIRQ('0'), UART_IRQ_OUTPUT),
                    pty.irq + IRQ_UART_PTY_BYTE_IN);
    avr_connect_irq(pty.irq + IRQ_UART_PTY_BYTE_OUT,
                    avr_io_getirq(avr, AVR_IOCTL_UART_GETIRQ('0'), UART_IRQ_INPUT));
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_UART_GETIRQ('0'), UART_IRQ_OUT_XON),
                            on_xon, NULL);
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_UART_GETIRQ('0'), UART_IRQ_OUT_XOFF),
                            on_xoff, NULL);
    signal(SIGTERM, on_terminate);
    printf("port %s\n", pty.pty.slavename);
    fflush(stdout);

    double start = now();
    avr_cycle_count_t next_check = THROTTLE_CYCLES;
    uint32_t previous_pc = avr->pc;
    while (!terminated && !dumped) {
        if (avr->pc == boot_start && application_blank())
            avr_regbit_set(avr, avr->reset_flags.extrf); // the reset button
        if (avr->pc < boot_start && previous_pc >= boot_start && !application_blank()) {
            dump();
            printf("application started\n");
            fflush(stdout);
            break;
        }
        previous_pc = avr->pc;
        int state = avr_run(avr);
        if (state == cpu_Done || state == cpu_Crashed) {
            fprintf(stderr, "simavr stopped at 0x%x (state %d)\n", avr->pc, state);
            dump();
            return 1;
        }
        if (avr->cycle >= next_check) {
            next_check = avr->cycle + THROTTLE_CYCLES;
            double ahead = (double)avr->cycle / FREQUENCY - (now() - start);
            if (ahead > 0)
                usleep((useconds_t)(ahead * 1e6));
        }
    }
    while (!terminated)
        usleep(10000);
    if (!dumped)
        dump();
    return 0;
}
