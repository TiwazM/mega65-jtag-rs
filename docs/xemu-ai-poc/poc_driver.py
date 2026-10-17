#!/usr/bin/env python3 -I
"""Proof-of-concept driver for the patched Xemu monitor (ai-poc branch).
Measures: frame-step latency, bulk memory read throughput, PNG screenshot time,
joystick override visibility at $DC00."""
import socket, sys, time, os

SOCK = sys.argv[1] if len(sys.argv) > 1 else '/tmp/xemu-poc.sock'
OUT = os.path.dirname(os.path.abspath(__file__))

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(SOCK)
s.settimeout(10)

def cmd(c):
    s.sendall((c + '\n').encode())
    buf = b''
    while not buf.endswith(b'.\r\n'):
        buf += s.recv(1 << 20)
    return buf.decode(errors='replace')

def timed(label, c, n=1):
    t = time.time()
    for _ in range(n):
        r = cmd(c)
    dt = (time.time() - t) / n * 1000
    print(f'{label:34s} {dt:8.2f} ms   -> {r.strip().splitlines()[-2][:70]!r}')
    return r

print('== baseline command latency (CPU running) ==')
timed('r  (registers)', 'r', 20)
timed('m  (16 bytes)', 'm0000', 20)

print('== frame stepping ==')
cmd('t1')                      # pause
timed('~frames 1 (realtime)', '~frames 1', 20)
timed('~frames A (10 frames, realtime)', '~frames A', 5)
cmd('~sleepless 1')
timed('~frames 1 (sleepless)', '~frames 1', 20)
timed('~frames A (10 frames, sleepless)', '~frames A', 10)
timed('~frames 3C (60 frames, sleepless)', '~frames 3C', 3)
cmd('~sleepless 0')

print('== bulk memory ==')
for ln in (0x100, 0x1000, 0x10000, 0x20000):
    t = time.time(); r = cmd(f'~mem 0 {ln:X}'); dt = time.time() - t
    hexlen = len(r.strip().splitlines()[-2])
    print(f'~mem 0 {ln:5X}  {ln/1024:6.0f} KB  {dt*1000:8.2f} ms  {ln/1024/dt:8.0f} KB/s  (got {hexlen//2} bytes)')

print('== screenshot ==')
path = os.path.join(OUT, 'poc-shot.png')
t = time.time(); r = cmd(f'~shot {path}'); cmd('~frames 1'); dt = time.time() - t
print(f'~shot + frame: {dt*1000:.1f} ms -> {r.strip().splitlines()[-2]}  exists={os.path.exists(path)} size={os.path.getsize(path) if os.path.exists(path) else 0}')

print('== joystick override, read back through CIA1 port A ($DC00, port 2) ==')
print('idle   :', cmd('m777DC00').strip().splitlines()[-2])
cmd('~joy2 1F'); cmd('~frames 1')
print('held   :', cmd('m777DC00').strip().splitlines()[-2])
cmd('~joy2 0'); cmd('~frames 1')
print('release:', cmd('m777DC00').strip().splitlines()[-2])

cmd('t0')
print('resumed; regs:', cmd('r').strip().splitlines()[-3][:60])
