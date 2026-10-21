"""Reload the matching installed SCP through its bootstrap; no SD replacement."""
import time
from pathlib import Path
import driver as d
R=Path('/Users/stevehanff/mega65-jtag-rs/hardware-tests/space-commander-v13/ship-dma/current')
print(d.cmd('reset'),flush=True);time.sleep(3)
print(d.cmd('prg',str(R/'space-commander.prg'),'--run'),flush=True)
boot=d.DATA['bootstrap'];until=time.monotonic()+120
while d.read(d.G['main_loop'],8).hex()!=d.DATA['fingerprints']['main_loop']:
    if d.value(boot['music_mode'])==1 and d.value(boot['menu_visible'])==1:d.cmd('type','~M')
    assert time.monotonic()<until,'Still in bootstrap'
    time.sleep(2)
until=time.monotonic()+30
while d.value(d.G['game_state'])!=1:
    assert time.monotonic()<until,'Cockpit not reached';time.sleep(.5)
d.verify();print('Current build restarted in combat.',flush=True)
