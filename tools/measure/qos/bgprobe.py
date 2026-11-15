# SPDX-License-Identifier: GPL-3.0-only
"""CPU time (share of one core) of a background-QoS process in 2 s wall time; the parent process measures and
kills it, so a starving child process does not hold up the measurement."""
import os, signal, subprocess, sys, time
code = "import ctypes\nctypes.CDLL('/usr/lib/libSystem.dylib').pthread_set_qos_class_self_np(%d, 0)\nwhile True: pass\n"
def share(qos):
    p = subprocess.Popen([sys.executable, "-I", "-c", code % qos])
    time.sleep(2.0)
    os.kill(p.pid, signal.SIGKILL)
    _, _, ru = os.wait4(p.pid, 0)
    return (ru.ru_utime + ru.ru_stime) / 2.0
print(f"bg {share(0x09):.2f} utility {share(0x11):.2f}")
