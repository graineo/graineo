# SPDX-License-Identifier: GPL-3.0-only
"""QoS probe: how much CPU time does a thread get per QoS class in 10 s (pure computation or reading a file)?"""
import ctypes, os, sys, threading, time, resource, mmap

libc = ctypes.CDLL("/usr/lib/libSystem.dylib")
QOS = {"background": 0x09, "utility": 0x11, "user_initiated": 0x19}


def thread_cpu():
    return time.thread_time()


def work(kind, qos, path, out):
    libc.pthread_set_qos_class_self_np(QOS[qos], 0)
    t0, c0 = time.time(), thread_cpu()
    n = 0
    if kind == "compute":
        while time.time() - t0 < 10:
            x = 0
            for i in range(20000):
                x += i * i
            n += 1
    else:
        # reading via mmap with page faults; evicting the file from the cache is not possible without root: read fresh locations
        with open(path, "rb") as f:
            m = mmap.mmap(f.fileno(), 0, prot=mmap.PROT_READ)
            size = len(m)
            off = 0
            while time.time() - t0 < 10 and off < size:
                _ = m[off]
                off += 4096
                n += 1
    out[(kind, qos)] = (thread_cpu() - c0, n, time.time() - t0)


if __name__ == "__main__":
    path = sys.argv[1]
    out = {}
    for kind in ["compute", "read"]:
        for qos in ["background", "utility"]:
            t = threading.Thread(target=work, args=(kind, qos, path, out))
            t.start(); t.join()
            cpu, n, wall = out[(kind, qos)]
            print(f"{kind:8s} {qos:15s} CPU {cpu:5.2f} s in {wall:5.1f} s wall, steps {n}", flush=True)
