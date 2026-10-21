"""Temporary frame-boundary A/B gate; no executable fixture injection."""
import json,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
gate=d.G['ship_dma_gate'];original=b'\xad'
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[],pixels=[],
    note='Same compiled renderer, fixed scenes; CPU reference disables only DMA eligibility. Serial monitor overhead included.')
def save():
    (R/'matched.json').write_text(json.dumps(report,indent=2)+'\n')
def mode(enabled):
    d.park();d.put(gate,original if enabled else b'\x60')
    assert d.read(gate)==(original if enabled else b'\x60')
    d.resume()
def pair(label,repeat=False,capture=False):
    for iteration in range(2 if repeat else 1):
        for enabled in (False,True):
            mode(enabled)
            row=d.measure(label+(' DMA' if enabled else ' CPU'),6)
            row.update(scene=label,enabled=enabled,iteration=iteration)
            report['measurements'].append(row);save()
            if capture and iteration==0:
                path=R/('pixel-after.png' if enabled else 'pixel-before.png')
                d.park()
                try:
                    print(d.cmd('screenshot','--png',str(path),timeout=30)[-150:],flush=True)
                    report['pixels'].append(dict(enabled=enabled,enemy_on_screen=d.value(d.G['enemy_on_screen'])))
                    save()
                finally:d.resume()
d.verify();assert d.value(d.G['sector_loaded'])==0
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
try:
    d.pose(1,160);pair('Mule 160 m',True,True)
    d.pose(0,600);pair('Raider 600 m',True)
    d.pose(0,1200);pair('Raider 1200 m')
    mode(True)
    d.park();d.setg('sector_stage',1);d.setg('bonus_active',1);d.setg('run_won',1)
    d.setg('explosion_ticks',1);d.setg('throttle',0);d.setg('game_tick',tick);d.resume()
    until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=1:
        assert time.monotonic()<until,'Sector 1 not ready';time.sleep(.25)
    d.cmd('type','h');until=time.monotonic()+30
    while d.value(d.G['sector_loaded'])!=2:
        assert time.monotonic()<until,'Sector 2 not ready';time.sleep(.25)
    d.carrier_pose(1500);pair('Carrier 1500 m',True)
finally:
    d.park();d.put(gate,original);d.setg('game_tick',tick)
    assert d.read(gate)==original and d.read(d.G['game_tick'])==tick
    d.resume();a=d.snapshot();time.sleep(2);b=d.snapshot()
    report['restored']=dict(gate_active=True,simulation_restored=True,
        rendered_delta=(b['rendered']-a['rendered'])&65535,
        presented_delta=(b['presented']-a['presented'])&65535)
    save();print('RESTORED',report['restored'],flush=True)
