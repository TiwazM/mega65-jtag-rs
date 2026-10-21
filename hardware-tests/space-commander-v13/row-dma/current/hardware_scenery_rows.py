"""Run on the Mac beside driver.py, symbols.json and assembled reference files.

Uses verified zero padding below the rotation tables, not live game code.
The reference changes only the copy loop after common clipping/address setup.
Every RAM change is read back; all patches are removed in finally.
"""
import json
import struct
import time
import driver as d

ref=json.loads((d.ROOT/'reference.json').read_text())
code=(d.ROOT/'reference.bin').read_bytes()
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
out=d.ROOT.parent
d.verify()
assert d.read(d.G['planet_dma_job'],16).hex()=='0b078600808081008200830184008501'
assert d.value(ref['line_mode'])==0x80
assert ref['at']==d.G['cine_code_end'] and ref['at']+len(code)<=0xb800

def checked_put(at,data):
    for pos in range(0,len(data),8):d.put(at+pos,data[pos:pos+8])
    assert d.read(at,len(data))==bytes(data),(hex(at),'write readback failed')

report=dict(scp_sha256=d.DATA['scp_sha256'],scenes=[])
backup=None
try:
    if d.value(d.G['sector_loaded'])!=2:
        d.park()
        for n,v in [('sector_stage',1),('bonus_active',1),('run_won',1),('explosion_ticks',1),('throttle',0)]:d.setg(n,v)
        d.setg('game_tick',tick);d.resume()
        deadline=time.monotonic()+30
        while d.value(d.G['sector_loaded'])!=1:
            assert time.monotonic()<deadline,'cleared trap load';time.sleep(.3)
        d.cmd('type','h')
        deadline=time.monotonic()+30
        while d.value(d.G['sector_loaded'])!=2:
            assert time.monotonic()<deadline,'carrier load';time.sleep(.3)
    d.carrier_pose(1500);d.park()
    backup=d.read(ref['at'],len(code));assert backup==bytes(len(code))
    checked_put(ref['at'],code)
    for distance in (4800,1500):
        d.resume();d.carrier_pose(distance);d.park()
        for label,enabled in [('reference',False),('optimized',True)]:
            checked_put(ref['patch_at'],bytes.fromhex(ref['entry']) if enabled else b'\x4c'+struct.pack('<H',ref['at']))
            checked_put(ref['line_mode'],[0x80 if enabled else 0])
            d.resume()
            report['scenes'].append(d.measure(f'{label} carrier {distance} m',8))
            d.park()
            if distance==1500:
                output=d.cmd('screenshot','--png',str(out/f'pixel-{label}.png'),timeout=40)
                print(output[-160:],flush=True)
    (out/'matched.json').write_text(json.dumps(report,indent=2))
finally:
    if backup is not None:
        checked_put(ref['patch_at'],bytes.fromhex(ref['entry']))
        checked_put(ref['line_mode'],[0x80])
        checked_put(ref['at'],backup)
    d.setg('game_tick',tick);d.resume()
    a=d.snapshot();time.sleep(2);b=d.snapshot()
    report['restored']=dict(rendered_delta=(b['rendered']-a['rendered'])&65535,
                           presented_delta=(b['presented']-a['presented'])&65535)
    assert report['restored']['rendered_delta']>0 and report['restored']['presented_delta']>0
    d.verify()
    (out/'matched.json').write_text(json.dumps(report,indent=2))
    print('Reference removed; production restored and frames advancing.',flush=True)
