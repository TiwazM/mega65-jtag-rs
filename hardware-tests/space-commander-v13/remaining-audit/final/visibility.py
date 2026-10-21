"""Read visibility at the frame breakpoint, before screenshot can resume CPU."""
import json,struct,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
d.cmd('reset');time.sleep(3);d.cmd('prg',str(R/'space-commander.prg'),'--run')
boot=d.DATA['bootstrap'];until=time.monotonic()+120
while d.read(d.G['main_loop'],8).hex()!=d.DATA['fingerprints']['main_loop']:
    if d.value(boot['music_mode'])==1 and d.value(boot['menu_visible'])==1:d.cmd('type','~M')
    assert time.monotonic()<until;time.sleep(2)
while d.value(d.G['game_state'])!=1:
    assert time.monotonic()<until;time.sleep(.5)
d.verify();d.park()
at=d.DATA['reference_start'];reference=(R/'reference.bin').read_bytes();oldhelp=d.read(at,len(reference))
entry=d.G['matrix_apply_rows'];original=d.read(entry,3);tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
for off in range(0,len(reference),24):d.put(at+off,reference[off:off+24])
d.resume();checks=[]
def compare(name):
    values=[]
    for enabled in (False,True):
        d.park();d.put(entry,original if enabled else b'\x4c'+struct.pack('<H',d.DATA['reference_rows']));d.resume();time.sleep(.6)
        d.park();values.append(d.value(d.G['enemy_on_screen']));d.resume()
    assert values==[1,1],(name,values)
    checks.append(dict(scene=name,visible=values));print(checks[-1],flush=True)
try:
    for kind,name,distance in ((1,'mule-close',160),(0,'raider',600),(2,'manta',260),(3,'razorwing',260)):
        d.pose(kind,distance);compare(name)
    d.park();d.setg('sector_stage',1);d.setg('bonus_active',1);d.setg('run_won',1)
    d.setg('explosion_ticks',1);d.setg('throttle',0);d.setg('game_tick',tick);d.resume()
    until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=1:
        assert time.monotonic()<until;time.sleep(.25)
    d.cmd('type','h');until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=2:
        assert time.monotonic()<until;time.sleep(.25)
    d.carrier_pose(1500);compare('carrier-close');d.carrier_pose(4800);compare('carrier-far')
    d.carrier_pose(1500)
finally:
    d.park();d.put(entry,original)
    for off in range(0,len(oldhelp),24):d.put(at+off,oldhelp[off:off+24])
    d.setg('game_tick',tick);d.resume();d.verify()
(R/'visibility.json').write_text(json.dumps(dict(passed=True,scp_sha256=d.DATA['scp_sha256'],checks=checks,code_restored=True),indent=2)+'\n')
