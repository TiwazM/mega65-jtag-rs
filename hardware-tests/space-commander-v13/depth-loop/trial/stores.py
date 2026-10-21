"""Compare unchanged-depth-store elimination with the original loop."""
import json,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[])
def save(): (R/'stores.json').write_text(json.dumps(report,indent=2)+'\n')
d.verify();assert d.value(d.G['sector_loaded'])==0;d.park()
patches={0x633b:(bytes.fromhex('2e'),bytes.fromhex('af')),
         0x6367:(bytes.fromhex('02'),bytes.fromhex('83')),
         0x62f0:(bytes.fromhex('0a'),bytes.fromhex('13')),
         0x62f8:(bytes.fromhex('02'),bytes.fromhex('0f')),
         0x62fb:(bytes.fromhex('9007eab202c51db00d'),bytes.fromhex('eab202c51db00f8008'))}
for a,(old,new) in patches.items():assert d.read(a,len(old))==old,(hex(a),d.read(a,len(old)).hex())
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
d.resume()
def mode(enabled):
    d.park()
    for a,(old,new) in patches.items():d.put(a,new if enabled else old)
    d.resume()
try:
    d.pose(1,160)
    for iteration in range(2):
        for enabled in ((False,True) if iteration==0 else (True,False)):
            mode(enabled);row=d.measure('Mule 160 m '+str(enabled),8)
            row.update(enabled=enabled,iteration=iteration);report['measurements'].append(row);save()
    for enabled in (False,True):
        mode(enabled);time.sleep(.6);d.park()
        report['visible_'+str(enabled)]=d.value(d.G['enemy_on_screen'])
        d.cmd('screenshot','--png',str(R/('stores-'+str(enabled)+'.png')),timeout=30);d.resume();save()
finally:
    mode(False);d.park();d.setg('game_tick',tick);d.resume();d.verify();report['restored']=True;save()
    print('Original depth loop restored.',flush=True)
