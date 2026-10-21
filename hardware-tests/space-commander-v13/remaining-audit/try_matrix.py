"""A/B a redundant sign-extension call without modifying the executable."""
import json,struct,sys,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
mode=sys.argv[1]
report=dict(scp_sha256=d.DATA['scp_sha256'],mode=mode,measurements=[],captures=[])
def save(): (R/('matrix-'+mode+'.json')).write_text(json.dumps(report,indent=2)+'\n')
d.verify();d.park()
start=d.G['matrix_apply'];body=d.read(start,d.G['wide_vertex_depth']-start)
call=b'\x20'+struct.pack('<H',d.G['math_signed'])
assert body.count(call)==1
addr=start+body.index(call)
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
report['patch_address']=addr
d.resume()
def variant(enabled):
    d.park();d.put(addr,b'\xea\xea\xea' if enabled else call);d.resume()
try:
    scenes=[(None,1500,'carrier-near'),(None,4800,'carrier-far')] if mode=='carrier' else [(1,160,'mule-near'),(0,600,'raider-mid')]
    for kind,distance,name in scenes:
        if kind is None: d.carrier_pose(distance)
        else:d.pose(kind,distance)
        for iteration in range(3):
            for enabled in ((False,True) if iteration%2==0 else (True,False)):
                variant(enabled)
                row=d.measure(name+(' short-product' if enabled else ' original'),6)
                row.update(scene=name,enabled=enabled,iteration=iteration)
                report['measurements'].append(row);save()
                if iteration==0:
                    d.park()
                    try:
                        png=R/(name+('-short.png' if enabled else '-original.png'))
                        d.cmd('screenshot','--png',str(png),timeout=30)
                        report['captures'].append(dict(scene=name,enabled=enabled,path=png.name,visible=d.value(d.G['enemy_on_screen'])))
                        save()
                    finally:d.resume()
finally:
    d.park();d.put(addr,call);d.setg('game_tick',tick);d.resume();d.verify()
    report['restored']=True;save();print('Trial restored.',flush=True)
