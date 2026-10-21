"""Real-MEGA65 A/B against the assembled previous production span."""
import json,struct,time
from pathlib import Path
import driver as d
from depth_reference import Reference
R=Path(__file__).resolve().parent
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[],captures=[])
def save(): (R/'verified.json').write_text(json.dumps(report,indent=2)+'\n')
d.verify();d.park()
ref=Reference(d.G,d.DATA['reference'],d.DATA['reference_start'],(R/'reference.bin').read_bytes(),d.read,d.put)
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1];d.resume()
def mode(enabled):d.park();ref.mode(enabled);d.resume()
def compare(name,measure=False):
    for enabled in (False,True):
        mode(enabled)
        if measure:
            row=d.measure(name+(' optimized' if enabled else ' original span'),8)
            row.update(scene=name,enabled=enabled);report['measurements'].append(row)
        else:time.sleep(.5)
        d.park();visible=d.value(d.G['enemy_on_screen'])
        p=R/(name+('-after.png' if enabled else '-before.png'))
        d.cmd('screenshot','--png',str(p),timeout=30)
        report['captures'].append(dict(scene=name,enabled=enabled,visible=visible,path=p.name));save();d.resume()
try:
    for kind,name,distance in ((1,'mule160',160),(1,'mule300',300),(0,'raider600',600),
                               (2,'manta260',260),(3,'razorwing260',260)):
        mode(True);d.pose(kind,distance);compare(name,kind in (0,1))
    mode(True);d.pose(1,160);d.park();d.put(0x187f,[0]);d.resume();compare('mule-flat',True)
    mode(True);d.park();d.put(0x187f,[1]);ref.restore()
    d.setg('sector_stage',1);d.setg('bonus_active',1);d.setg('run_won',1);d.setg('explosion_ticks',1)
    d.setg('throttle',0);d.setg('game_tick',tick);d.resume()
    until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=1:
        assert time.monotonic()<until;time.sleep(.25)
    d.cmd('type','h');until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=2:
        assert time.monotonic()<until;time.sleep(.25)
    d.park();ref=Reference(d.G,d.DATA['reference'],d.DATA['reference_start'],(R/'reference.bin').read_bytes(),d.read,d.put);d.resume()
    d.carrier_pose(1500);compare('carrier1500',True)
finally:
    d.park();ref.restore();d.put(0x187f,[1]);d.setg('game_tick',tick);d.resume();d.verify()
    a=d.snapshot();time.sleep(2);b=d.snapshot()
    report['restored']=dict(code=True,reference_removed=True,simulation=True,
        rendered_delta=(b['rendered']-a['rendered'])&65535,presented_delta=(b['presented']-a['presented'])&65535)
    save();print('RESTORED',report['restored'],flush=True)
