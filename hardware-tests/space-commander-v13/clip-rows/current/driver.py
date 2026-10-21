"""Hardware audit fixtures; production files are never modified.

Run on the Mac next to symbols.json. RAM fixtures use a main-loop breakpoint,
and restore code before returning. Results are fixed-scene costs, not gameplay
averages. The serial monitor itself has overhead.
"""
import argparse
import json
import math
from pathlib import Path
import re
import statistics
import struct
import subprocess
import time

ROOT = Path(__file__).resolve().parent
DATA = json.loads((ROOT / 'symbols.json').read_text())
G, F, Q = (DATA[n] for n in ('game', 'fleet', 'carrier'))
EXE = '/Users/stevehanff/mega65-jtag-rs/target/release/m65rs'

def cmd(*args, timeout=30):
    p = subprocess.run([EXE, '--device', '/dev/cu.usbserial-101', *map(str, args)],
                       capture_output=True, text=True, timeout=timeout)
    if p.returncode:
        raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p.stdout

def read(addr, n=1):
    out = cmd('dump', f'{addr:x}', n)
    raw = b''.join(bytes.fromhex(m[1]) for line in out.splitlines()
                  if (m := re.match(r'^[0-9a-fA-F]+: ((?:[0-9a-fA-F]{2} )+)', line)))
    assert len(raw) == n, (n, out)
    return raw

def value(addr, n=1):
    return int.from_bytes(read(addr, n), 'little')

def put(addr, data):
    cmd('poke', f'{addr:x}', *(f'{v:02x}' for v in data))

def setg(name, data):
    put(G[name], bytes([data]) if isinstance(data, int) else data)

def park():
    cmd('break', f'{G["main_loop"]:x}', '--timeout', 5)
    cmd('break', 'clear')

def resume():
    cmd('go')

def verify():
    assert value(G['game_state']) == 1, 'Expected a running cockpit'
    for name, expected in DATA['fingerprints'].items():
        assert read(G[name], len(bytes.fromhex(expected))).hex() == expected, name
    assert value(0x187f) == 1, 'Expected HardStep'

def snapshot():
    t0 = time.monotonic()
    a = read(G['presented_frames'], 4)
    t1 = time.monotonic()
    b = read(G['vblank'], 19)
    u16 = lambda pos: int.from_bytes(b[pos:pos+2], 'little')
    return dict(t=(t0+t1)/2, read_seconds=t1-t0,
                presented=int.from_bytes(a[:2], 'little'),
                replaced=int.from_bytes(a[2:], 'little'),
                video=u16(1), rendered=u16(3), draw_ms=u16(7)/4,
                loop_ms=u16(13)/4, hud_fps=b[16], hud_frame_ms=b[17])

def measure(label, seconds=5):
    time.sleep(1.5)
    rows = [snapshot()]
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        time.sleep(.35)
        rows.append(snapshot())
    elapsed = rows[-1]['t'] - rows[0]['t']
    result = dict(label=label, seconds=elapsed,
                  presented_fps=((rows[-1]['presented']-rows[0]['presented']) & 65535)/elapsed,
                  rendered_fps=((rows[-1]['rendered']-rows[0]['rendered']) & 65535)/elapsed,
                  video_hz=((rows[-1]['video']-rows[0]['video']) & 65535)/elapsed,
                  median_draw_ms=statistics.median(r['draw_ms'] for r in rows),
                  draw_ms_range=[min(r['draw_ms'] for r in rows), max(r['draw_ms'] for r in rows)],
                  median_hud_fps=statistics.median(r['hud_fps'] for r in rows), rows=rows)
    assert 55 < result['video_hz'] < 65, result
    print(json.dumps({k:v for k,v in result.items() if k != 'rows'}), flush=True)
    return result

def pose(kind, distance):
    park()
    setg('game_tick', 0x60)
    put(F['orient_q'], struct.pack('<4h', 16384, 0, 0, 0))
    put(F['body_matrix'], struct.pack('<9h', 256,0,0,0,256,0,0,0,256))
    for name in ('player_yaw','player_pitch','throttle','explosion_ticks','enemy_flash',
                 'incoming_ticks','missile_ticks','hull_flash','impact_ticks','barrier_flash',
                 'laser_pending','fleet_active','current_slot','selected_slot',
                 'enemy_barrier','weapon_mode','beam_ticks','show_stats'):
        setg(name, 0)
    setg('enemy_kind', kind)
    setg('enemy_shields', 20)
    setg('enemy_bank', 0)
    setg('enemy_pos', struct.pack('<3h', 0,0,distance))
    setg('enemy_heading', [64,16])
    resume()

def carrier_pose(distance):
    park()
    setg('game_tick', 0x60)
    put(Q['orient_q'], struct.pack('<4h',16384,0,0,0))
    put(Q['body_matrix'], struct.pack('<9h',256,0,0,0,256,0,0,0,256))
    put(Q['carrier_attitude'], struct.pack('<9h',0,0,256,0,256,0,-256,0,0))
    setg('enemy_pos', struct.pack('<3h',0,0,distance))
    for name in ('player_yaw','player_pitch','throttle','show_stats','missile_ticks','beam_ticks'):
        setg(name,0)
    put(Q['carrier_panel_key'],[255])
    put(Q['carrier_call_age'],[0])
    resume()

def probe(label, addr):
    park()
    original=read(addr)
    try:
        put(addr,[0x60])
        resume()
        return measure(label)
    finally:
        park()
        put(addr,original)
        resume()

def save(report, name):
    (ROOT/name).write_text(json.dumps(report,indent=2)+'\n')

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('mode',choices=['combat','carrier'])
    args=ap.parse_args()
    verify()
    tick=bytes.fromhex(DATA['fingerprints']['game_tick'])[:1]
    report=dict(scp_sha256=DATA['scp_sha256'],gameplay=False,
                note='Production renderer with temporary, fixed-scene RAM fixtures. Code restored; serial monitor has overhead.',
                scene_measurements=[],omission_probes=[])
    try:
        if args.mode=='combat':
            assert value(G['sector_loaded'])==0
            for kind,distance,label in [(0,-600,'ship behind camera'),(0,600,'Raider 600 m'),
                                        (0,300,'Raider 300 m'),(1,160,'Mule 160 m')]:
                pose(kind,distance)
                report['scene_measurements'].append(measure(label))
                save(report,'combat.json')
            report['profile']=cmd('profile',6,'--bucket',16,'--top',18,timeout=15)
            print(report['profile'],flush=True)
            for name in ('flight_planet','composite_enemy','draw_flight_hud'):
                report['omission_probes'].append(probe('Mule 160 m: temporarily omit '+name,G[name]))
                save(report,'combat.json')
            report['repeat']=measure('Mule 160 m: all drawing restored')
            save(report,'combat.json')
        else:
            # Prepare the existing post-mine progression, then use the real H
            # input and loader. This does not change the release unlock rule.
            park()
            setg('game_tick',tick)
            setg('sector_stage',1)
            setg('bonus_active',1)
            setg('run_won',1)
            setg('explosion_ticks',1)
            setg('throttle',0)
            resume()
            time.sleep(.8)
            cmd('type','h')
            deadline=time.monotonic()+20
            while value(G['sector_loaded'])!=2:
                assert time.monotonic()<deadline,'Carrier sector did not load'
                time.sleep(.3)
            for distance in (4800,1500):
                carrier_pose(distance)
                report['scene_measurements'].append(measure(f'Carrier broadside {distance} m with sky'))
                save(report,'carrier.json')
            report['profile']=cmd('profile',6,'--bucket',16,'--top',18,timeout=15)
            print(report['profile'],flush=True)
            for name,table in [('sector_sky',Q),('carrier_frame',Q)]:
                report['omission_probes'].append(probe('Carrier 1500 m: temporarily omit '+name,table[name]))
                save(report,'carrier.json')
            report['repeat']=measure('Carrier 1500 m: all drawing restored')
            save(report,'carrier.json')
    finally:
        park()
        setg('game_tick',tick)
        resume()
        print('Simulation code restored; game resumed.',flush=True)

if __name__=='__main__':
    main()
