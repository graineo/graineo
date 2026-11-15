#!/usr/bin/env python3
"""Timestamp per line: reads stdin, writes `<monotonic in ms>\t<line>` to stdout.

Part of tools/measure/timing (timing for the project website). The app writes its timings
(`GRAINEO_LOG=1`) line by line and unbuffered to stderr; this filter gives every line a common clock,
so that lines without their own stamp (e.g. `job … : 2s`) are accurate to the millisecond as well.
License: GPL-3.0-only.
"""
import sys
import time

for line in iter(sys.stdin.readline, ""):
    sys.stdout.write(f"{time.monotonic() * 1000.0:.1f}\t{line}")
    sys.stdout.flush()
