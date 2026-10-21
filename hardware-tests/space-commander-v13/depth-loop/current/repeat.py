"""Reverse-order confirmation of the carrier timing after pixel validation."""
import json,time
from pathlib import Path
import driver as d
from depth_reference import Reference
R=Path(__file__).resolve().parent
d.verify();assert d.value(d.G['sector_loaded'])==2;d.carrier_pose(1500);d.park()
ref=Reference(d.G,d.DATA['reference'],d.DATA['reference_start'],(R/'reference.bin').read_bytes(),d.read,d.put)
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1];d.resume()
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[])
try:
    for enabled in (True,False,True):
        d.park();ref.mode(enabled);d.resume()
        row=d.measure('carrier1500 reverse-order '+str(enabled),8)
        row['enabled']=enabled;report['measurements'].append(row)
finally:
    d.park();ref.restore();d.setg('game_tick',tick);d.resume();d.verify()
    report['restored']=True;(R/'repeat.json').write_text(json.dumps(report,indent=2)+'\n')
    print('Normal simulation and compiled renderer restored.',flush=True)
