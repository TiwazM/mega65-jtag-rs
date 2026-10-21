"""Hardware image checks at unequal tile alignments and cockpit overlaps."""
import json,struct,time
from pathlib import Path
import driver as d
R=Path(__file__).resolve().parent;g=d.G
result=dict(scp_sha256=d.DATA['scp_sha256'],checks=[])
def check_palette():
    pal=d.read(0xffd3100,768)
    expected=(R/'sector.pal').read_bytes()
    assert b''.join(pal[c*256+192:(c+1)*256] for c in range(3))==expected,'Sector 02 palette changed'
d.verify();assert d.value(g['sector_loaded'])==2
try:
    d.carrier_pose(1500)
    check_palette()
    for label,pos,flash in [('unaligned',(137,47,1500),0),('right-frame',(1450,0,1500),0),
                             ('upper-monitor',(1100,600,1500),0),('hull-hit',(0,0,1500),8)]:
        row=dict(scene=label,visible=[])
        for enabled in (False,True):
            d.park();d.setg('enemy_pos',struct.pack('<3h',*pos));d.setg('hull_flash',flash)
            d.put(g['ship_dma_gate'],b'\xad' if enabled else b'\x60');d.resume();time.sleep(.7);d.park()
            row['visible'].append(d.value(g['enemy_on_screen']))
            check_palette()
            path=R/(label+('-after.png' if enabled else '-before.png'))
            print(d.cmd('screenshot','--png',str(path),timeout=30)[-120:],flush=True)
            d.resume()
        assert row['visible'][0]==row['visible'][1],row
        result['checks'].append(row)
finally:
    d.park();d.put(g['ship_dma_gate'],b'\xad');d.setg('hull_flash',0)
    d.setg('enemy_pos',struct.pack('<3h',0,0,1500));d.setg('game_tick',bytes.fromhex(d.DATA['fingerprints']['game_tick'])[:1]);d.resume()
    a=d.snapshot();time.sleep(2);b=d.snapshot()
    check_palette();result['sector_palette_preserved']=True
    result['restored']=dict(rendered_delta=(b['rendered']-a['rendered'])&65535,presented_delta=(b['presented']-a['presented'])&65535)
    (R/'edges.json').write_text(json.dumps(result,indent=2)+'\n')
    print('Restored',result['restored'],flush=True)
