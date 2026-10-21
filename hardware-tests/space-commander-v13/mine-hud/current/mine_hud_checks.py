"""Pixel reference for mine-panel transitions, shared by Xemu and hardware."""
from pathlib import Path
import re

def font(root):
    source=(Path(root)/'src/hud_font.a').read_text()
    index=[int(v) for v in source.split('!byte ',1)[1].splitlines()[0].split(',')]
    glyphs=bytes(int(v,16) for v in re.findall(r'\$([0-9a-fA-F]{2})',source))
    return index,glyphs

def expected_panel(font_data,left=None,selected=255,ready=False):
    pixels=bytearray(91*51)
    def dot(x,y,ink=9): pixels[(y-140)*91+x-14]=ink
    def text(words,x,y,ink=9):
        index,glyphs=font_data
        for char in words:
            rows=glyphs[index[ord(char)-32]*6:][:6]
            for dy,bits in enumerate(rows):
                for dx in range(4):
                    if bits&(128>>dx):dot(x+dx,y+dy,ink)
            x+=5
    def line(x,y,u,v):
        dx,dy=abs(u-x),abs(v-y);sx=1 if x<u else -1;sy=1 if y<v else -1
        err=dx-dy
        while True:
            dot(x,y)
            if (x,y)==(u,v):break
            e=err*2
            if e>-dy:err-=dy;x+=sx
            if e<dx:err+=dx;y+=sy
    if ready:
        text('TRAP CLEARED',20,145);text('H  HYPERSPACE',20,161,5)
    elif left is None:
        text('NO TARGET',20,142,7)
    else:
        text(f'LEFT {left:02d}',20,142)
        if selected<12:
            text(f'MINE {selected+1:02d}',20,153)
            dot(62,170)
            xs=[62,65,76,68,76,65,62,59,48,56,48,59,62,68,62,56,62]
            ys=[162,167,164,170,176,173,178,173,176,170,164,167,162,170,173,170,162]
            for i in range(16):line(xs[i],ys[i],xs[i+1],ys[i+1])
        text('SHOOT TO CLEAR',20,181)
    return bytes(pixels)

def read_panel(read,slot=None):
    screen=read(0x10000+slot*2048,2000) if slot is not None else None
    cache={};pixels=bytearray()
    for y in range(140,191):
        for x in range(14,105):
            tile=y//8*40+x//8
            addr=int.from_bytes(screen[tile*2:tile*2+2],'little')*64 if screen else 0x20000+tile*64
            block=addr&~255
            if block not in cache:cache[block]=read(block,256)
            pixels.append(cache[block][(addr&255)+(y%8)*8+x%8])
    return bytes(pixels)

def check(read,font_data,label,left=None,selected=255,ready=False,slots=3):
    want=expected_panel(font_data,left,selected,ready)
    checked=[]
    for slot in [None]+list(range(slots)):
        actual=read_panel(read,slot)
        diffs=[(14+i%91,140+i//91,a,b) for i,(a,b) in enumerate(zip(actual,want)) if a!=b]
        assert not diffs,(label,slot,len(diffs),diffs[:12])
        checked.append('source' if slot is None else slot)
    result=dict(scene=label,left=left,selected=selected,checked=checked,changed_pixels=0)
    print('PASS',result,flush=True)
    return result
