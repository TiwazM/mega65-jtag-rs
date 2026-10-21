"""Temporary real-hardware stage probes; restore every instruction in finally."""
import json,sys,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent
mode=sys.argv[1]
report=dict(scp_sha256=d.DATA['scp_sha256'],mode=mode,scenes=[])
def save(): (R/(mode+'.json')).write_text(json.dumps(report,indent=2)+'\n')
d.verify()
tick=bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]
try:
    if mode=='carrier':
        assert d.value(d.G['sector_loaded'])==2
        scenes=[(None,1500,'Carrier 1500 m'),(None,4800,'Carrier 4800 m')]
    else:
        assert d.value(d.G['sector_loaded'])==0
        scenes=[(1,160,'Mule 160 m'),(0,600,'Raider 600 m')]
    for kind,distance,name in scenes:
        if kind is None: d.carrier_pose(distance)
        else: d.pose(kind,distance)
        row=dict(name=name,baseline=d.measure(name,6),probes={})
        report['scenes'].append(row);save()
        row['pc_profile']=d.cmd('profile',8,'--bucket',16,'--top',30,timeout=20)
        print(row['pc_profile'],flush=True);save()
        stages=['render_faces','transform_vertices','prepare_faces','composite_enemy','draw_flight_hud']
        if kind is None: stages+=['sector_sky']
        else: stages+=['flight_planet']
        for name in stages:
            addr=(d.Q if name=='sector_sky' else d.G)[name]
            row['probes'][name]=d.probe(row['name']+' omit '+name,addr);save()
        row['restored_baseline']=d.measure(row['name']+' restored',6);save()
finally:
    d.park();d.setg('game_tick',tick);d.resume()
    d.verify();report['code_restored']=True;save()
    print('All instructions restored; game running.',flush=True)
