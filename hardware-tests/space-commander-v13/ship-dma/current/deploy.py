"""Load matching SCP/bootstrap, then identify actual game code before fixtures."""
import hashlib,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
assert hashlib.sha256((R/'SPACE.SCP').read_bytes()).hexdigest()==d.DATA['scp_sha256']
for c in [('reset',),('sd','--keep','put',str(R/'SPACE.SCP'),'/SPACE.SCP'),('reset',),('prg',str(R/'space-commander.prg'),'--run')]:
    print(c[0],d.cmd(*c,timeout=150)[-150:],flush=True);time.sleep(3)
boot=d.DATA['bootstrap'];until=time.monotonic()+120
while d.read(d.G['main_loop'],8).hex()!=d.DATA['fingerprints']['main_loop']:
    if d.value(boot['music_mode'])==1 and d.value(boot['menu_visible'])==1:d.cmd('type','~M')
    assert time.monotonic()<until,'Still in bootstrap'
    time.sleep(2)
until=time.monotonic()+30
while d.value(d.G['game_state'])!=1:
    assert time.monotonic()<until,'Cockpit not reached';time.sleep(.5)
d.verify();print('Current compiled game verified in cockpit.',flush=True)
