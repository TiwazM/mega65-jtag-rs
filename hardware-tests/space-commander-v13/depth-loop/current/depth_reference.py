"""Test-only original span in inactive reset code, with all patch sites routed.

Call only at a parked frame boundary. Restore before transitions/new games.
No precision, pixel coverage, colour or depth logic is simulated here: the
reference is the assembled previous production routine.
"""
import struct

class Reference:
    def __init__(self,g,refs,at,blob,read,put):
        self.g,self.refs,self.at,self.blob,self.read,self.put=g,refs,at,blob,read,put
        assert at+len(blob)<=g['game_input']
        self.saved=read(at,len(blob));self.entry=read(g['depth_span'],3)
        for off in range(0,len(blob),24):put(at+off,blob[off:off+24])
        self.ranges=[(g['configure_depth_loop'],g['depth_span']),
                     (g['band_next'],g['band_left']), (0xee00,0xefd0)]

    def mode(self,enabled):
        targets=('depth_step_frac','depth_step_lo','depth_step_hi','vp_advance_gate')
        for start,end in self.ranges:
            data=self.read(start,end-start)
            for name in targets:
                a,b=self.g[name]+1,self.refs[name]+1
                for old in (a,b):
                    pattern=b'\x8d'+struct.pack('<H',old)
                    pos=0
                    while (pos:=data.find(pattern,pos))!=-1:
                        self.put(start+pos+1,struct.pack('<H',a if enabled else b));pos+=3
        self.put(self.g['depth_span'],self.entry if enabled else b'\x4c'+struct.pack('<H',self.refs['depth_span']))

    def restore(self):
        self.mode(True)
        for off in range(0,len(self.saved),24):self.put(self.at+off,self.saved[off:off+24])
