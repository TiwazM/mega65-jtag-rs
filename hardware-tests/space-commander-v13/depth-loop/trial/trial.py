"""Measure byte-sized branch routing changes, restoring every byte afterward."""
import json,struct,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[],patches={})
def save(): (R/'trial.json').write_text(json.dumps(report,indent=2)+'\n')

# Restart the matching, already installed SCP so the fleet overlay is loaded.
d.cmd('reset');time.sleep(3)
d.cmd('prg','/Users/stevehanff/mega65-jtag-rs/hardware-tests/space-commander-v13/remaining-audit/final/space-commander.prg','--run');time.sleep(3)
until=time.monotonic()+100
while d.read(d.G['main_loop'],8).hex()!=d.DATA['fingerprints']['main_loop']:
    boot=d.DATA['bootstrap']
    if d.value(boot['music_mode'])==1 and d.value(boot['menu_visible'])==1:d.cmd('type','~M')
    assert time.monotonic()<until
    time.sleep(2)
while d.value(d.G['game_state'])!=1:
    assert time.monotonic()<until;time.sleep(.5)
d.verify();d.park()
assert d.read(0x633a,2)==bytes.fromhex('d02e')
assert d.read(0x6366,2)==bytes.fromhex('9002')
assert d.read(0x62ef,2)==bytes.fromhex('900a')
assert d.read(0x62f7,2)==bytes.fromhex('9002')
changes={0x633b:(0x2e,0xaf),0x6367:(2,0x83),0x62f0:(0x0a,0x13),0x62f8:(2,0x0b)}
report['patches']={hex(a):list(v) for a,v in changes.items()}
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
d.resume()
def mode(name):
    d.park()
    for a,(old,new) in changes.items():
        active=name=='both' or (name=='loop' and a>=0x633b)
        d.put(a,[new if active else old])
    d.resume()
try:
    d.pose(1,160)
    for iteration,order in enumerate((('baseline','loop','both'),('both','loop','baseline'))):
        for name in order:
            mode(name);row=d.measure('Mule 160 m '+name,7)
            row.update(mode=name,iteration=iteration,scene='mule160');report['measurements'].append(row);save()
    for kind,distance,scene in ((1,300,'mule300'),(0,600,'raider600')):
        d.pose(kind,distance)
        for name in ('baseline','both'):
            mode(name);row=d.measure(scene+' '+name,7)
            row.update(mode=name,scene=scene);report['measurements'].append(row);save()
finally:
    mode('baseline');d.park();d.setg('game_tick',tick);d.resume();d.verify()
    report['restored']=True;save();print('Original code and simulation restored.',flush=True)
