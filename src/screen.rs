//! Capture the VIC-IV text display: as coloured terminal text, or rendered
//! pixel-accurately (text modes, incl. 16-bit/FCM/NCM chars) into a PNG.
//! This follows mega65-tools' screen_shot.c.

use std::io::Write;

use crate::machine::{COLOUR_RAM, VIC_BASE};
use crate::monitor::Monitor;
use crate::petscii::screencode_to_char;
use crate::Result;

/// Pipeline delay: the 800 pixel wide raster is centred in the 720 pixel image.
const SCREEN_POSITION: i32 = (800 - 720) / 2;

pub struct VideoState {
    /// $D000-$D0FF, then the palette ($D100-$D3FF) at 0x100, alt palette at 0x400.
    pub regs: Vec<u8>,
    pub screen_addr: u32,
    pub charset_addr: u32,
    pub colour_addr: u32,
    pub line_step: usize,
    pub width: usize,
    pub rows: usize,
    pub sixteen_bit: bool,
    pub upper_case: bool,
    pub pal: bool,
    pub h640: bool,
    pub v400: bool,
    pub ebc: bool,
    pub multicolour: bool,
    pub bitmap: bool,
    pub viciii_attribs: bool,
    pub border: u8,
    pub background: u8,
    pub screen: Vec<u8>,
    pub colour: Vec<u8>,
    pub charset: Vec<u8>,
    /// Full-colour glyph and bitmap data, in 256-byte blocks keyed by address,
    /// read during `capture` so a paused capture covers it too.
    glyphs: std::collections::HashMap<u32, Vec<u8>>,
    /// The screen pointer ($D060-$D062) changed while capturing: the program
    /// flips between screen buffers, so an unpaused capture mixes two frames.
    pub flipped: bool,
    chargen_x: i32,
    chargen_y: i32,
    top_border: i32,
    bottom_border: i32,
    left_border: i32,
    right_border: i32,
    y_scale: u32,
    x_step: f32,
}

impl VideoState {
    /// Read everything needed to draw the screen. The CPU should be stopped by the
    /// caller for a consistent snapshot.
    pub fn capture(mon: &mut Monitor) -> Result<Self> {
        let mut regs = mon.read_memory(VIC_BASE, 0x100)?;
        // Map the palette banks we need via $D070 and read them.
        let palreg = regs[0x70];
        let altpalsel = palreg & 3;
        let btpalsel = (palreg >> 4) & 3;
        let mappedpal = palreg >> 6;
        if mappedpal != btpalsel {
            mon.poke(VIC_BASE + 0x70, (palreg & 0x3f) | (btpalsel << 6))?;
        }
        let pal = mon.read_memory(VIC_BASE + 0x100, 0x300)?;
        regs.extend_from_slice(&pal);
        if btpalsel != altpalsel {
            mon.poke(VIC_BASE + 0x70, (palreg & 0x3f) | (altpalsel << 6))?;
            let alt = mon.read_memory(VIC_BASE + 0x100, 0x300)?;
            regs.extend_from_slice(&alt);
        } else {
            regs.extend_from_slice(&pal);
        }
        if mappedpal != btpalsel || mappedpal != altpalsel {
            mon.poke(VIC_BASE + 0x70, palreg)?;
        }

        let r = &regs;
        let screen_addr = r[0x60] as u32 | (r[0x61] as u32) << 8 | (r[0x62] as u32) << 16;
        let mut charset_addr = r[0x68] as u32 | (r[0x69] as u32) << 8 | (r[0x6a] as u32) << 16;
        charset_addr = match charset_addr {
            0x1000 => 0x2d000,
            0x9000 => 0x29000,
            0x1800 => 0x2d800,
            0x9800 => 0x29800,
            a => a,
        };
        let line_step = (r[0x58] as usize) | (r[0x59] as usize) << 8;
        let width = r[0x5e] as usize;
        let rows = 1 + r[0x7b] as usize;
        let sixteen_bit = r[0x54] & 1 != 0;
        let screen_size = line_step * rows * if sixteen_bit { 2 } else { 1 };
        if screen_size > 128 * 1024 || width == 0 || rows == 0 {
            return Err(crate::Error::Other(format!("implausible screen geometry: {width}x{rows}, step {line_step}")));
        }
        let charset_size = if sixteen_bit && r[0x54] & 4 == 0 { 8192 * 8 } else { 2048 };
        // Physical lines per glyph row: CHRYSCL+1, doubled when the VIC-III V400
        // flag is set (measured on an R6: V400=0/CHRYSCL=1 and V400=1/CHRYSCL=0
        // both give 16-line character rows).
        let v400 = r[0x31] & 0x08 != 0;
        let y_scale = (r[0x5b] as u32 + 1) * if v400 { 2 } else { 1 } - 1;
        let h640 = r[0x31] & 0x80 != 0;
        let mut x_step = r[0x5a] as f32 / 120.0;
        if !h640 {
            x_step /= 2.0;
        }
        let side = ((r[0x5c] as i32) | (r[0x5d] as i32) << 8) & 0xfff;

        let screen = mon.read_memory(screen_addr, screen_size)?;
        let colour_addr = (r[0x64] as u32) | (r[0x65] as u32) << 8;
        let colour = mon.read_memory(COLOUR_RAM + colour_addr, screen_size)?;
        let charset = mon.read_memory(charset_addr, charset_size)?;

        let mut vs = VideoState {
            screen_addr,
            charset_addr,
            colour_addr,
            line_step,
            width,
            rows,
            sixteen_bit,
            upper_case: r[0x18] & 2 == 0,
            pal: r[0x6f] & 0x80 == 0,
            h640,
            v400,
            ebc: r[0x11] & 0x40 != 0,
            multicolour: r[0x16] & 0x10 != 0,
            bitmap: r[0x11] & 0x20 != 0,
            viciii_attribs: r[0x31] & 0x20 != 0,
            border: r[0x20],
            background: r[0x21],
            chargen_x: (((r[0x4c] as i32) | (r[0x4d] as i32) << 8) & 0xfff) - SCREEN_POSITION,
            chargen_y: ((r[0x4e] as i32) | (r[0x4f] as i32) << 8) & 0xfff,
            top_border: ((r[0x48] as i32) | (r[0x49] as i32) << 8) & 0xfff,
            bottom_border: ((r[0x4a] as i32) | (r[0x4b] as i32) << 8) & 0xfff,
            left_border: side - SCREEN_POSITION,
            right_border: 800 - side - SCREEN_POSITION,
            y_scale,
            x_step,
            screen,
            colour,
            charset,
            glyphs: Default::default(),
            flipped: false,
            regs,
        };
        vs.fetch_glyphs(mon)?;
        let ptr = mon.read_memory(VIC_BASE + 0x60, 3)?;
        vs.flipped = ptr[..] != vs.regs[0x60..0x63];
        Ok(vs)
    }

    /// Read the full-colour glyphs and bitmap bytes the screen refers to, merging
    /// neighbouring 256-byte blocks into one read.
    fn fetch_glyphs(&mut self, mon: &mut Monitor) -> Result<()> {
        let mut blocks = std::collections::BTreeSet::new();
        for cy in 0..self.rows {
            let mut fcm_yoffs: i32 = 0;
            for cx in 0..self.width {
                let (ch, col) = self.cell(cx, cy);
                if col & 0x1000 != 0 {
                    let yo = ((ch >> 13) & 7) as i32;
                    fcm_yoffs = if ch & 0x1000 != 0 { -yo } else { yo };
                    continue;
                }
                let char_id = if self.ebc { (ch & 0x3f) as usize } else { (ch & 0x1fff) as usize };
                for row in 0..8 {
                    if self.full_colour(char_id) {
                        blocks.insert(self.fcm_addr(char_id, row, fcm_yoffs) & !0xff);
                    } else if self.bitmap {
                        blocks.insert(self.bitmap_addr(cx, cy, row) & !0xff);
                    }
                }
            }
        }
        let blocks: Vec<u32> = blocks.into_iter().collect();
        let mut i = 0;
        while i < blocks.len() {
            let mut j = i + 1;
            while j < blocks.len() && blocks[j] == blocks[j - 1] + 0x100 {
                j += 1;
            }
            let data = mon.read_memory(blocks[i], (j - i) * 0x100)?;
            for (k, chunk) in data.chunks(0x100).enumerate() {
                self.glyphs.insert(blocks[i + k], chunk.to_vec());
            }
            i = j;
        }
        Ok(())
    }

    fn full_colour(&self, char_id: usize) -> bool {
        (self.regs[0x54] & 2 != 0 && char_id < 0x100) || (self.regs[0x54] & 4 != 0 && char_id > 0xff)
    }

    fn fcm_addr(&self, char_id: usize, row: usize, fcm_yoffs: i32) -> u32 {
        (char_id as i64 * 64 + (row as i64 + fcm_yoffs as i64) * 8).max(0) as u32
    }

    fn bitmap_addr(&self, cx: usize, cy: usize, row: usize) -> u32 {
        if self.h640 {
            (self.charset_addr & 0xfc000) + (cx * 8 + cy * 640 + row) as u32
        } else {
            (self.charset_addr & 0xfe000) + (cx * 8 + cy * 320 + row) as u32
        }
    }

    pub fn height(&self) -> usize {
        if self.pal { 576 } else { 480 }
    }

    /// Palette entry as (r,g,b). The VIC-IV stores nibbles swapped.
    pub fn rgb(&self, colour: u8, alt: bool) -> (u8, u8, u8) {
        let base = if alt { 0x400 } else { 0x100 };
        let f = |i: usize| {
            let v = self.regs[base + i * 0x100 + colour as usize];
            (v & 0xf) << 4 | (v >> 4)
        };
        (f(0), f(1), f(2))
    }

    fn cell(&self, x: usize, y: usize) -> (u16, u16) {
        let w = if self.sixteen_bit { 2 } else { 1 };
        let i = y * self.line_step + x * w;
        let mut ch = self.screen.get(i).copied().unwrap_or(0) as u16;
        let mut col = self.colour.get(i).copied().unwrap_or(0) as u16;
        if self.sixteen_bit {
            ch |= (self.screen.get(i + 1).copied().unwrap_or(0) as u16) << 8;
            col = (col << 8) | self.colour.get(i + 1).copied().unwrap_or(0) as u16;
        }
        (ch, col)
    }

    /// Plain text rendering (no colour), one line per screen row.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for y in 0..self.rows {
            for x in 0..self.width {
                let (ch, _) = self.cell(x, y);
                let fcm = (self.regs[0x54] & 2 != 0 && ch & 0x1fff < 0x100) || (self.regs[0x54] & 4 != 0 && ch & 0x1fff > 0xff);
                s.push(if fcm { '?' } else { screencode_to_char((ch & 0xff) as u8, self.upper_case) });
            }
            s.push('\n');
        }
        s
    }

    /// ANSI true-colour rendering for a terminal, with a one character border.
    pub fn to_ansi(&self) -> String {
        let mut s = String::new();
        let (br, bg, bb) = self.rgb(self.border, false);
        let border_line = format!("\x1b[48;2;{br};{bg};{bb}m{}\x1b[0m\n", " ".repeat(self.width + 2));
        s.push_str(&border_line);
        for y in 0..self.rows {
            s.push_str(&format!("\x1b[48;2;{br};{bg};{bb}m "));
            for x in 0..self.width {
                let (ch, col) = self.cell(x, y);
                let (char_id, cell_bg) = if self.ebc {
                    ((ch & 0x3f) as usize, self.regs[0x21 + ((ch >> 6) & 3) as usize])
                } else {
                    ((ch & 0x1fff) as usize, self.background)
                };
                let mut fg = (col & 0xff) as u8;
                let (mut bold, mut reverse) = (false, false);
                if self.viciii_attribs && !self.multicolour {
                    reverse = col & 0x20 != 0;
                    bold = col & 0x40 != 0;
                    if bold && !reverse {
                        fg |= 0x10;
                    }
                }
                let alt = bold && reverse;
                let fcm = (self.regs[0x54] & 2 != 0 && char_id < 0x100) || (self.regs[0x54] & 4 != 0 && char_id > 0xff) || col & 0x800 != 0;
                let (f, b) = if reverse && !bold { (cell_bg, fg) } else { (fg, cell_bg) };
                let (fr, fgc, fb) = self.rgb(f, alt);
                let (bgr, bgg, bgb) = self.rgb(b, alt);
                s.push_str(&format!("\x1b[48;2;{bgr};{bgg};{bgb}m\x1b[38;2;{fr};{fgc};{fb}m"));
                if fcm {
                    s.push('?');
                } else {
                    let c = (char_id & 0xff) as u8;
                    if c & 0x80 != 0 {
                        s.push_str("\x1b[7m");
                    }
                    s.push(screencode_to_char(c, self.upper_case));
                    if c & 0x80 != 0 {
                        s.push_str("\x1b[27m");
                    }
                }
            }
            s.push_str(&format!("\x1b[48;2;{br};{bg};{bb}m \x1b[0m\n"));
        }
        s.push_str(&border_line);
        s
    }

    /// Render to a 720 x (576|480) RGB framebuffer. Glyph data `capture` did not
    /// fetch is read from the machine on demand through `mon`.
    pub fn render(&self, mon: &mut Monitor) -> Result<Vec<u8>> {
        let h = self.height();
        let mut fb = vec![0u8; 720 * h * 3];
        let (br, bg, bb) = self.rgb(self.border, false);
        for px in fb.chunks_mut(3) {
            px.copy_from_slice(&[br, bg, bb]);
        }
        let (kr, kg, kb) = self.rgb(self.background, false);
        for y in self.top_border.max(0)..self.bottom_border.min(h as i32) {
            for x in self.left_border.max(0)..self.right_border.min(720) {
                let o = (y as usize * 720 + x as usize) * 3;
                fb[o..o + 3].copy_from_slice(&[kr, kg, kb]);
            }
        }
        let mut set = |x: i32, y: i32, c: (u8, u8, u8)| {
            if x < 0 || x >= 720 || y < 0 || y >= h as i32 {
                return;
            }
            let o = (y as usize * 720 + x as usize) * 3;
            fb[o..o + 3].copy_from_slice(&[c.0, c.1, c.2]);
        };

        let mut fcm_cache = self.glyphs.clone();
        let mut y_pos = self.chargen_y;
        for cy in 0..self.rows {
            if y_pos >= h as i32 {
                break;
            }
            let mut x_pos = self.chargen_x;
            // GOTOX tokens set these for the characters that follow them.
            let mut transparent_bg = false;
            let mut fcm_yoffs: i32 = 0;
            let px_per_vic2 = if self.h640 { 1 } else { 2 };
            for cx in 0..self.width {
                let (ch, col) = self.cell(cx, cy);
                if col & 0x1000 != 0 {
                    // GOTOX: 10-bit signed X in VIC-II pixels, relative to the
                    // start of character drawing; transparent flag; FCM Y offset.
                    let raw = (ch & 0x3ff) as i32;
                    let gx = if raw & 0x200 != 0 { raw - 0x400 } else { raw };
                    x_pos = self.chargen_x + gx * px_per_vic2;
                    transparent_bg = col & 0x8000 != 0;
                    let yo = ((ch >> 13) & 7) as i32;
                    fcm_yoffs = if ch & 0x1000 != 0 { -yo } else { yo };
                    continue;
                }
                let mut background = self.background;
                let char_id = if self.ebc {
                    background = self.regs[0x21 + ((ch >> 6) & 3) as usize];
                    (ch & 0x3f) as usize
                } else {
                    (ch & 0x1fff) as usize
                };
                let mut width_deduct = (ch >> 13) as usize;
                let mut fg = (col & 0x0f) as u8;
                let flip_v = col & 0x8000 != 0;
                let flip_h = col & 0x4000 != 0;
                let with_alpha = col & 0x2000 != 0;
                let (mut bold, mut reverse, mut underline) = (false, false, false);
                if self.viciii_attribs && !self.multicolour {
                    reverse = col & 0x20 != 0;
                    bold = col & 0x40 != 0;
                    underline = col & 0x80 != 0;
                    if bold && !reverse {
                        fg |= 0x10;
                    }
                }
                let alt = bold && reverse;
                if self.multicolour {
                    fg = (col & 0xff) as u8;
                }
                let mut bitmap_multi = 0u8;
                if self.bitmap {
                    let v = self.screen[cy * self.line_step + cx * if self.sixteen_bit { 2 } else { 1 }];
                    fg = v & 0xf;
                    background = v >> 4;
                    bitmap_multi = self.colour[cy * self.line_step + cx * if self.sixteen_bit { 2 } else { 1 }];
                }
                let full_colour = self.full_colour(char_id);
                let four_bit = col & 0x800 != 0;
                if col & 0x400 != 0 {
                    width_deduct += 8;
                }
                let glyph_width = (if four_bit { 16usize } else { 8 }).saturating_sub(width_deduct);
                let mut xc = 0i32;
                for yy in 0..8i32 {
                    let row = if flip_v { 7 - yy } else { yy } as usize;
                    let mut g = [0u8; 8];
                    if full_colour {
                        let addr = self.fcm_addr(char_id, row, fcm_yoffs);
                        let base = addr & !0xff;
                        if !fcm_cache.contains_key(&base) {
                            fcm_cache.insert(base, mon.read_memory(base, 256)?);
                        }
                        let o = (addr - base) as usize;
                        g.copy_from_slice(&fcm_cache[&base][o..o + 8]);
                    } else if !self.bitmap {
                        let b = self.charset.get(char_id * 8 + row).copied().unwrap_or(0);
                        for (i, gi) in g.iter_mut().enumerate() {
                            *gi = if (b >> i) & 1 != 0 { 0xff } else { 0 };
                        }
                    } else {
                        let addr = self.bitmap_addr(cx, cy, row);
                        let base = addr & !0xff;
                        if !fcm_cache.contains_key(&base) {
                            fcm_cache.insert(base, mon.read_memory(base, 256)?);
                        }
                        let b = fcm_cache[&base][(addr - base) as usize];
                        for (i, gi) in g.iter_mut().enumerate() {
                            *gi = if (b >> i) & 1 != 0 { 0xff } else { 0 };
                        }
                    }
                    if flip_h {
                        g.reverse();
                    }
                    if reverse && !bold {
                        for v in g.iter_mut() {
                            *v = 0xff - *v;
                        }
                    }
                    if underline && yy == 7 {
                        g = [0xff; 8];
                    }
                    xc = 0;
                    let mut xx = 0f32;
                    while xx < glyph_width as f32 {
                        let xi = xx as usize;
                        let mut c = self.rgb(background, alt);
                        let mut is_fg = false;
                        if four_bit {
                            let mut v = g[xi / 2];
                            v = if xi & 1 != 0 { v >> 4 } else { v & 0xf };
                            if with_alpha {
                                let f = self.rgb(fg, alt);
                                let a = v as u32;
                                c = (
                                    ((f.0 as u32 * a + c.0 as u32 * (15 - a)) / 15) as u8,
                                    ((f.1 as u32 * a + c.1 as u32 * (15 - a)) / 15) as u8,
                                    ((f.2 as u32 * a + c.2 as u32 * (15 - a)) / 15) as u8,
                                );
                            } else {
                                match v {
                                    0 => {}
                                    0xf => c = self.rgb(fg, alt),
                                    _ => c = self.rgb(v, alt),
                                }
                            }
                            is_fg = v != 0;
                        } else if full_colour {
                            if with_alpha {
                                let f = self.rgb(fg, alt);
                                let a = g[xi] as u32;
                                c = (
                                    ((f.0 as u32 * a + c.0 as u32 * (255 - a)) >> 8) as u8,
                                    ((f.1 as u32 * a + c.1 as u32 * (255 - a)) >> 8) as u8,
                                    ((f.2 as u32 * a + c.2 as u32 * (255 - a)) >> 8) as u8,
                                );
                                is_fg = fg != 0;
                            } else {
                                c = self.rgb(g[xi], alt);
                                is_fg = g[xi] != 0;
                            }
                        } else if self.multicolour && (fg & 8 != 0 || self.bitmap) {
                            let mut bits = 0;
                            if g[6 - (xi & 6)] != 0 {
                                bits |= 1;
                            }
                            if g[7 - (xi & 6)] != 0 {
                                bits |= 2;
                            }
                            let colour = if !self.bitmap {
                                match bits {
                                    0 => self.regs[0x21],
                                    1 => {
                                        is_fg = true;
                                        self.regs[0x22]
                                    }
                                    2 => {
                                        is_fg = true;
                                        self.regs[0x23]
                                    }
                                    _ => {
                                        is_fg = true;
                                        fg & 7
                                    }
                                }
                            } else {
                                match bits {
                                    0 => {
                                        is_fg = true;
                                        self.regs[0x21]
                                    }
                                    1 => background,
                                    2 => {
                                        is_fg = true;
                                        fg
                                    }
                                    _ => {
                                        is_fg = true;
                                        bitmap_multi & 0xf
                                    }
                                }
                            };
                            c = self.rgb(colour, alt);
                        } else if g[7 - xi.min(7)] != 0 {
                            c = self.rgb(fg, alt);
                            is_fg = true;
                        }
                        if transparent_bg && !is_fg {
                            xc += 1;
                            xx += self.x_step;
                            continue;
                        }
                        for yc in 0..=self.y_scale as i32 {
                            let py = y_pos + yc + yy * (1 + self.y_scale as i32);
                            let px = x_pos + xc;
                            if py < self.bottom_border && py >= self.top_border && px < self.right_border && px >= self.left_border {
                                set(px, py, c);
                            }
                        }
                        xc += 1;
                        xx += self.x_step;
                    }
                }
                x_pos += xc;
            }
            y_pos += 8 * (1 + self.y_scale as i32);
        }
        Ok(fb)
    }

    /// Render and encode as PNG.
    pub fn to_png(&self, mon: &mut Monitor, out: &mut dyn Write) -> Result<()> {
        let fb = self.render(mon)?;
        let mut enc = png::Encoder::new(out, 720, self.height() as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| crate::Error::Other(e.to_string()))?;
        w.write_image_data(&fb).map_err(|e| crate::Error::Other(e.to_string()))?;
        Ok(())
    }
}
