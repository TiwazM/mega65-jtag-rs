"""Real-machine regression fixtures; restore all simulation code on exit."""
import json,struct,time
import driver as d
from mine_hud_checks import check
B=d.DATA['bonus'];font_data=json.loads((d.ROOT/'font.json').read_text());checks=[]
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:3]

def wait(fn,label,timeout=30):
    until=time.monotonic()+timeout
    while not fn():
        assert time.monotonic()<until,label
        time.sleep(.1)
    print(label,flush=True)

def settle():
    d.resume();time.sleep(.6);d.park()

def panel(label,left=None,selected=255,ready=False):
    # Coalesce adjacent serial reads. The parked scene is immutable.
    cache={}
    def read(addr,n):
        out=bytearray()
        while n:
            block=addr&~2047;offset=addr-block;take=min(n,2048-offset)
            if block not in cache:cache[block]=d.read(block,2048)
            out+=cache[block][offset:offset+take];addr+=take;n-=take
        return bytes(out)
    checks.append(check(read,font_data,label,left,selected,ready))

def capture(name):
    d.cmd('screenshot','--png',str(d.ROOT/(name+'.png')),timeout=45)
    d.park() # screenshot resumes the processor; never assume it remains halted.

def last_fixture():
    d.put(B['orient_q'],struct.pack('<4h',16384,0,0,0))
    d.put(B['body_matrix'],struct.pack('<9h',256,0,0,0,256,0,0,0,256))
    d.put(B['mine_status'],[255]+[0]*11);d.put(B['mine_left'],[1])
    d.put(B['mine_pos'],struct.pack('<3h',0,0,200));d.put(B['mine_frac'],[0]*72)
    d.put(B['mine_heading'],[0]*12);d.put(B['mine_pitch'],[0]*12)
    d.setg('selected_slot',0);d.setg('heat',0);d.setg('throttle',0)
    settle()

try:
    d.verify();d.pose(3,600);d.park();d.setg('enemy_shields',0);d.setg('explosion_ticks',1);settle()
    panel('combat-no-target')
    d.setg('kills',[21,0]);d.setg('comms_seen',1);d.setg('game_tick',tick);d.resume()
    wait(lambda:d.value(d.G['bonus_active'])==1,'defeat call started')
    wait(lambda:d.value(B['mine_phase'])==1,'mine phase started')
    d.park();d.setg('game_tick',0x60);d.setg('throttle',0);settle()
    panel('combat-to-mines',12,d.value(d.G['selected_slot']));capture('mine-entry')
    last_fixture();panel('last-mine-alive',1,0)
    d.setg('laser_pending',1);settle()
    assert d.value(B['mine_left'])==0 and d.value(B['mine_status'])>0
    assert d.value(d.G['selected_slot'])==255 and d.value(d.G['sector_stage'])==0
    panel('last-laser-mine-exploding',0,255);capture('last-laser-mine')
    # The other kill path happens in simulation before compact_restore.
    last_fixture();d.put(B['mine_pos'],struct.pack('<3h',0,0,70));d.setg('game_tick',tick);d.resume()
    wait(lambda:d.value(B['mine_left'])==0,'proximity mine exploded')
    d.park();d.setg('game_tick',0x60);settle()
    assert d.value(d.G['sector_stage'])==0 and d.value(d.G['selected_slot'])==255
    panel('last-proximity-mine-exploding',0,255);capture('last-proximity-mine')
    d.setg('game_tick',tick);d.resume()
    wait(lambda:d.value(d.G['sector_loaded'])==1,'hyperspace unlocked')
    d.park();d.setg('game_tick',0x60);settle();panel('trap-cleared',ready=True);capture('trap-cleared')
    d.setg('game_tick',tick);d.resume();d.cmd('type','h')
    wait(lambda:d.value(d.G['sector_loaded'])==2,'carrier reached',30)
    d.carrier_pose(1500);d.park();d.setg('show_stats',1)
finally:
    d.park();d.setg('game_tick',tick);d.setg('throttle',0);d.verify();d.resume()
    print('Normal simulation restored.',flush=True)
before=d.snapshot();time.sleep(.5);after=d.snapshot()
report=dict(passed=True,scp_sha256=d.DATA['scp_sha256'],checks=checks,code_verified=True,simulation_restored=True,
            rendered_delta=(after['rendered']-before['rendered'])&65535,presented_delta=(after['presented']-before['presented'])&65535)
assert report['rendered_delta']>0 and report['presented_delta']>0
(d.ROOT/'mine-hud-hardware.json').write_text(json.dumps(report,indent=2)+'\n')
print('Hardware mine-panel checks passed.',flush=True)
