"""Validate the compiled transform against the previous routine on hardware."""
import json,struct,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[],captures=[])
def save(): (R/'verified.json').write_text(json.dumps(report,indent=2)+'\n')
d.verify();d.park()
at=d.DATA['reference_start'];reference=(R/'reference.bin').read_bytes()
assert len(reference)<256
oldhelp=d.read(at,len(reference));entry=d.G['matrix_apply_rows'];original=d.read(entry,3)
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
for off in range(0,len(reference),24):d.put(at+off,reference[off:off+24])
d.resume()
def mode(enabled):
    d.park();d.put(entry,original if enabled else b'\x4c'+struct.pack('<H',d.DATA['reference_rows']));d.resume()
def compare(name,measure=False):
    for iteration in range(2 if measure else 1):
        for enabled in ((False,True) if iteration==0 else (True,False)):
            mode(enabled)
            if measure:
                row=d.measure(name+(' current' if enabled else ' reference'),7)
                row.update(scene=name,enabled=enabled,iteration=iteration);report['measurements'].append(row);save()
            else:time.sleep(.5)
            if iteration==0:
                d.park()
                try:
                    path=R/(name+('-after.png' if enabled else '-before.png'))
                    visible=d.value(d.G['enemy_on_screen'])
                    d.cmd('screenshot','--png',str(path),timeout=30)
                    report['captures'].append(dict(scene=name,enabled=enabled,path=path.name,visible=visible));save()
                finally:d.resume()
try:
    for kind,name,distance in ((1,'mule-close',160),(0,'raider',600),(2,'manta',260),(3,'razorwing',260)):
        d.pose(kind,distance);compare(name,kind in (0,1))
    mode(True);d.park();d.setg('sector_stage',1);d.setg('bonus_active',1);d.setg('run_won',1)
    d.setg('explosion_ticks',1);d.setg('throttle',0);d.setg('game_tick',tick);d.resume()
    until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=1:
        assert time.monotonic()<until;time.sleep(.25)
    d.cmd('type','h');until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=2:
        assert time.monotonic()<until;time.sleep(.25)
    d.carrier_pose(1500);compare('carrier-close',True)
    d.carrier_pose(4800);compare('carrier-far',True)
finally:
    d.park();d.put(entry,original)
    for off in range(0,len(oldhelp),24):d.put(at+off,oldhelp[off:off+24])
    d.setg('game_tick',tick);d.resume();d.verify()
    a=d.snapshot();time.sleep(2);b=d.snapshot()
    report['restored']=dict(code=True,reference_removed=True,simulation=True,
        rendered_delta=(b['rendered']-a['rendered'])&65535,presented_delta=(b['presented']-a['presented'])&65535)
    save();print('RESTORED',report['restored'],flush=True)
