"""Measure the newly compiled direct-address depth loop on real hardware."""
import json,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
report=dict(scp_sha256=d.DATA['scp_sha256'],measurements=[])
d.verify();assert d.value(d.G['sector_loaded'])==0
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
try:
    for kind,distance,scene in ((1,160,'mule160'),(1,300,'mule300'),(0,600,'raider600'),(1,160,'mule160-repeat')):
        d.pose(kind,distance);row=d.measure(scene,9);row['scene']=scene;report['measurements'].append(row)
        (R/'direct.json').write_text(json.dumps(report,indent=2)+'\n')
finally:
    d.park();d.setg('game_tick',tick);d.resume();d.verify();report['restored']=True
    (R/'direct.json').write_text(json.dumps(report,indent=2)+'\n')
