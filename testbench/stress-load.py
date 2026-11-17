#!/usr/bin/env python3
"""Deterministic full-screen terminal damage at 60 Hz, no external packages."""
import shutil
import sys
import time

frame = 0
try:
    sys.stdout.write('\x1b[?25l')
    while True:
        start = time.monotonic()
        cols, rows = shutil.get_terminal_size()
        lines = []
        for y in range(max(1, rows - 1)):
            colour = 16 + ((y + frame) % 216)
            text = ''.join(chr(33 + ((x * 7 + y * 13 + frame) % 90))
                           for x in range(max(1, cols - 1)))
            lines.append(f'\x1b[48;5;{colour}m{text}\x1b[0m')
        sys.stdout.write('\x1b[H' + '\r\n'.join(lines))
        sys.stdout.flush()
        frame += 1
        time.sleep(max(0, 1 / 60 - (time.monotonic() - start)))
except (BrokenPipeError, KeyboardInterrupt):
    pass
