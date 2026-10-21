"""Real MEGA65 A/B: switch only clip-table data at a frame boundary."""
import json
import struct
import time
import driver as d

fixture=json.loads((d.ROOT/'clip-tables.json').read_text())
start=fixture['start'];old=bytes.fromhex(fixture['before']);new=bytes.fromhex(fixture['after'])
out=d.ROOT.parent
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
assert fixture['scp_sha256']==d.DATA['scp_sha256']
d.verify();assert d.read(start,len(new))==new,'Must boot the compiled new build first'
report=dict(scp_sha256=fixture['scp_sha256'],before_scp_sha256=fixture['before_scp_sha256'],
            note='All executable instructions are byte-identical. Only clipping data switches; fixed NTSC/triple/HardStep views.',scenes=[])
halted=False
def park():
    global halted
    if not halted:d.park();halted=True
def resume():
    global halted
    d.resume();halted=False
def tables(data):
    park()
    for offset in range(0,len(data),8):
        if old[offset:offset+8]!=new[offset:offset+8]:d.put(start+offset,data[offset:offset+8])
    assert d.read(start,len(data))==data
try:
    if d.value(d.G['sector_loaded'])!=2:
        park()
        for n,v in [('sector_stage',1),('bonus_active',1),('run_won',1),('explosion_ticks',1),('throttle',0)]:d.setg(n,v)
        d.setg('game_tick',tick);resume()
        deadline=time.monotonic()+30
        while d.value(d.G['sector_loaded'])!=1:
            assert time.monotonic()<deadline,'cleared trap load';time.sleep(.3)
        d.cmd('type','h');deadline=time.monotonic()+30
        while d.value(d.G['sector_loaded'])!=2:
            assert time.monotonic()<deadline,'carrier load';time.sleep(.3)
    for distance in (4800,1500):
        d.carrier_pose(distance);park()
        d.put(d.Q['sector_world'],struct.pack('<6h',-1600,450,4096,3300,-850,6500))
        for label,data,seconds in [('before',old,8),('after',new,8),('before-repeat',old,5),('after-repeat',new,5)]:
            tables(data);resume()
            report['scenes'].append(d.measure(f'{label} carrier {distance} m',seconds))
            park()
            (out/'matched.json').write_text(json.dumps(report,indent=2))
            if distance==1500 and label in ('before','after'):
                output=d.cmd('screenshot','--png',str(out/f'pixel-{label}.png'),timeout=40)
                print(output[-160:],flush=True)
        resume()
finally:
    tables(new);d.setg('game_tick',tick);resume()
    a=d.snapshot();time.sleep(2);b=d.snapshot()
    report['restored']=dict(rendered_delta=(b['rendered']-a['rendered'])&65535,
                           presented_delta=(b['presented']-a['presented'])&65535,
                           new_tables_active=d.read(start,len(new))==new)
    assert report['restored']['rendered_delta']>0 and report['restored']['presented_delta']>0
    d.verify();(out/'matched.json').write_text(json.dumps(report,indent=2))
    print('New clipping tables active; simulation restored and frames advancing.',flush=True)
