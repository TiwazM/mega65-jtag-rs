#!/usr/bin/env python3 -I
"""Regression test for the ai-agent Xemu branch: pipelined commands, delayed answers,
~shot semantics, pause/step/resume, bulk read speed."""
import socket, sys, time, os

SOCK = sys.argv[1] if len(sys.argv) > 1 else '/tmp/xemu-poc.sock'
OUT = os.path.dirname(os.path.abspath(__file__))
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(SOCK); s.settimeout(10)

def recv_answers(n):
    buf = b''
    while buf.count(b'.\r\n') < n:
        buf += s.recv(1 << 20)
    return buf.decode(errors='replace')

def cmd(c):
    s.sendall((c + '\n').encode()); return recv_answers(1)

def last(r): return r.strip().splitlines()[-2]

ok = True
def check(name, cond, detail=''):
    global ok
    ok &= bool(cond); print(('PASS' if cond else 'FAIL'), name, detail)

# 1. pipelining: 20 commands in one write, 20 answers, in order
t = time.time(); s.sendall(b''.join(f'm{i*16:04X}\n'.encode() for i in range(20))); r = recv_answers(20); dt = time.time() - t
addrs = [l[1:9] for l in r.splitlines() if l.startswith(':')]
check('20 pipelined m commands answered in order', addrs == [f'{i*16:08X}' for i in range(20)], f'{dt*1000:.0f} ms total')

# 2. CRLF terminated commands and an empty line while running (empty = step only when paused -> harmless)
s.sendall(b'r\r\nr\r\n'); r = recv_answers(2); check('CRLF-terminated commands', r.count('PC   A') == 2)

# 3. pause, delayed answers (~frames) followed by queued commands in the same write
cmd('t1')
t = time.time(); s.sendall(b'~frames 1\nr\nm0000\n'); r = recv_answers(3); dt = time.time() - t
check('~frames then queued r/m answered after the frame', r.count('PC   A') == 2 and ':00000000:' in r, f'{dt*1000:.0f} ms')

# 4. ~shot answers at once; file exists after the next ~frames
p = os.path.join(OUT, 'branch-shot.png')
if os.path.exists(p): os.remove(p)
r = cmd(f'~shot {p}'); check('~shot answers immediately', 'SHOT QUEUED' in r, last(r)[:40])
check('file not yet written while paused', not os.path.exists(p))
cmd('~frames 1'); check('file written after ~frames 1', os.path.exists(p) and os.path.getsize(p) > 500, f'{os.path.getsize(p) if os.path.exists(p) else 0} bytes')

# 5. step and resume
r1 = last(cmd('r')); r2 = last(cmd('')); check('single step changes PC', r1 != r2, f'{r1} -> {r2}')
cmd('~sleepless 1'); t = time.time(); cmd('~frames 64'); dt = time.time() - t; check('100 frames sleepless', dt < 1.0, f'{dt*1000:.0f} ms')
cmd('~sleepless 0')

# 6. bulk read speed
t = time.time(); r = cmd('~mem 0 20000'); dt = time.time() - t
check('~mem 128 KB', len(last(r)) == 0x40000, f'{dt*1000:.0f} ms = {128/dt:.0f} KB/s')

cmd('t0'); time.sleep(0.3); r = cmd('r'); check('resumed and answering', 'PC   A' in r)
print('ALL OK' if ok else 'SOME FAILED')
