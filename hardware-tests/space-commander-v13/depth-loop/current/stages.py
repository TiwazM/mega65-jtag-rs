"""Temporary omission probes to distinguish row setup from pixel work."""
import json,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
d.verify();tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
report=dict(scp_sha256=d.DATA['scp_sha256'],note='Omission deltas are non-additive upper bounds, not promised savings.',measurements=[])
try:
    d.pose(1,160);report['measurements'].append(d.measure('Mule160 complete',6))
    for name in ('depth_read_hi','depth_span','render_faces'):
        report['measurements'].append(d.probe('Mule160 omit '+name,d.G[name]))
    report['profile']=d.cmd('profile',6,'--bucket',16,'--top',18,timeout=15)
    print(report['profile'],flush=True)
finally:
    d.park();d.setg('game_tick',tick);d.resume();d.verify()
    report['restored']=True;(R/'stages.json').write_text(json.dumps(report,indent=2)+'\n')
