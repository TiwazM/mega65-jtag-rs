"""Run beside the matching driver.py/symbols.json on the hardware Mac."""
import argparse,json,struct,time
from pathlib import Path
import driver as d

def pair():
    d.pose(1,220);d.park()
    lo=d.read(d.F['fleet_addr_lo'],66);hi=d.read(d.F['fleet_addr_hi'],66)
    addresses=[lo[i]+256*hi[i] for i in range(66)];mem={}
    for name,data in [('enemy_pos',struct.pack('<3h',95,0,350)),('enemy_kind',b'\x02'),
                      ('enemy_shields',b'\x14'),('enemy_heading',bytes([64,16]))]:
        mem.update({d.G[name]+i:v for i,v in enumerate(data)})
    d.put(d.F['fleet_saved'],bytes(mem.get(a,0) for a in addresses))
    d.setg('enemy_pos',struct.pack('<3h',-35,0,220));d.setg('fleet_active',1);d.resume()

def sector():
    d.park();d.setg('game_tick',bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1])
    for name,value in [('fleet_active',0),('sector_stage',1),('bonus_active',1),
                       ('run_won',1),('explosion_ticks',1),('throttle',0)]:d.setg(name,value)
    d.resume();time.sleep(1);d.cmd('type','h');limit=time.monotonic()+25
    while d.value(d.G['sector_loaded'])!=2:
        assert time.monotonic()<limit,'Carrier load timed out';time.sleep(.5)
    d.carrier_pose(1500)

def sample_pose(x,y,z):
    d.park();d.setg('enemy_pos',struct.pack('<3h',x,y,z));d.resume();time.sleep(.15);d.park()
    return dict(position=[x,y,z],camera=list(struct.unpack('<3h',d.read(d.G['enemy_camera'],6))),
                screen=list(struct.unpack('<2h',d.read(d.G['enemy_screen_x'],4))))

if __name__=='__main__':
    ap=argparse.ArgumentParser();ap.add_argument('mode',choices=['combat','carrier']);a=ap.parse_args()
    d.park();frozen=d.read(d.G['game_tick'])==b'\x60'
    if frozen:d.setg('game_tick',bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1])
    d.verify()
    if frozen:d.setg('game_tick',0x60)
    d.resume();r=dict(scp_sha256=d.DATA['scp_sha256'],fixed_scene=True,rows=[])
    try:
        if a.mode=='combat':
            pair();r['rows'].append(d.measure('Mule 220 m + Manta 350 m',8))
            r['rows'].append(d.probe('Pair: omit target HUD',d.F['target_hud']))
            d.pose(0,600);r['rows'].append(d.measure('Raider 600 m',6))
            d.pose(1,160);r['rows'].append(d.measure('Mule 160 m',6))
        else:
            if d.value(d.G['sector_loaded'])!=2:sector()
            d.carrier_pose(1500);r['rows'].append(d.measure('Carrier broadside 1500 m',6))
            r['motion']=[sample_pose(300,180,z) for z in range(1504,1487,-1)]
            d.resume()
    finally:
        # Leave the fixed scene running while the rest of this audit continues.
        # Final verification explicitly restores game_tick before user play.
        d.resume()
        (d.ROOT/('target-'+a.mode+'.json')).write_text(json.dumps(r,indent=2)+'\n')
        print('Saved',a.mode,'fixture; simulation remains frozen for follow-up.',flush=True)
