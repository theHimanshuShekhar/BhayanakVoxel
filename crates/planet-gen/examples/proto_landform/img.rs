//! Tiny image toolkit: buffer, PNG writer, 3x5 bitmap font, legends, resizing, colour ramps.

pub type Rgb = [u8; 3];

#[derive(Clone)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgb>,
}

pub fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
pub fn to_rgb(c: [f32; 3]) -> Rgb {
    [c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8]
}
pub fn f3(c: Rgb) -> [f32; 3] {
    [f32::from(c[0]), f32::from(c[1]), f32::from(c[2])]
}

/// Piecewise-linear colour ramp.
pub fn ramp(stops: &[(f32, [f32; 3])], v: f32) -> [f32; 3] {
    if v <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if v <= w[1].0 {
            let t = (v - w[0].0) / (w[1].0 - w[0].0);
            return lerp3(w[0].1, w[1].1, t);
        }
    }
    stops[stops.len() - 1].1
}

impl Img {
    pub fn new(w: usize, h: usize, c: Rgb) -> Img {
        Img { w, h, px: vec![c; w * h] }
    }
    pub fn from_px(w: usize, h: usize, px: Vec<Rgb>) -> Img {
        Img { w, h, px }
    }
    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.px[yy * self.w + xx] = c;
            }
        }
    }
    pub fn blit(&mut self, src: &Img, x: usize, y: usize) {
        for yy in 0..src.h {
            if y + yy >= self.h {
                break;
            }
            for xx in 0..src.w {
                if x + xx < self.w {
                    self.px[(y + yy) * self.w + x + xx] = src.px[yy * src.w + xx];
                }
            }
        }
    }
    pub fn save(&self, path: &std::path::Path) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut enc = png::Encoder::new(file, self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let flat: Vec<u8> = self.px.iter().flat_map(|p| *p).collect();
        enc.write_header().unwrap().write_image_data(&flat).unwrap();
    }
    /// Area-average resize.
    pub fn resized(&self, w: usize, h: usize) -> Img {
        let mut out = vec![[0u8; 3]; w * h];
        for y in 0..h {
            let (y0, y1) = (y * self.h / h, ((y + 1) * self.h).div_ceil(h).min(self.h));
            for x in 0..w {
                let (x0, x1) = (x * self.w / w, ((x + 1) * self.w).div_ceil(w).min(self.w));
                let mut s = [0u32; 3];
                let mut n = 0;
                for yy in y0..y1.max(y0 + 1) {
                    for xx in x0..x1.max(x0 + 1) {
                        let p = self.px[yy.min(self.h - 1) * self.w + xx.min(self.w - 1)];
                        for c in 0..3 {
                            s[c] += u32::from(p[c]);
                        }
                        n += 1;
                    }
                }
                out[y * w + x] = [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8];
            }
        }
        Img { w, h, px: out }
    }

    pub fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: Rgb) {
        let mut cx = x;
        for ch in s.chars() {
            if let Some(g) = glyph(ch.to_ascii_uppercase()) {
                for (i, b) in g.bytes().enumerate() {
                    if b == b'#' {
                        self.rect(cx + (i % 3) * scale, y + (i / 3) * scale, scale, scale, c);
                    }
                }
            }
            cx += 4 * scale;
        }
    }
    pub fn text_width(s: &str, scale: usize) -> usize {
        s.chars().count() * 4 * scale
    }
}

fn glyph(c: char) -> Option<&'static str> {
    Some(match c {
        'A' => "####.#####.##.#",
        'B' => "##.#.###.#.###.",
        'C' => "####..#..#..###",
        'D' => "##.#.##.##.###.",
        'E' => "####..##.#..###",
        'F' => "####..##.#..#..",
        'G' => "####..#.##.####",
        'H' => "#.##.#####.##.#",
        'I' => "###.#..#..#.###",
        'J' => "..#..#..##.####",
        'K' => "#.##.###.#.##.#",
        'L' => "#..#..#..#..###",
        'M' => "#.########.##.#",
        'N' => "##.#.##.##.##.#",
        'O' => "####.##.##.####",
        'P' => "####.#####..#..",
        'Q' => "####.##.####..#",
        'R' => "####.###.#.##.#",
        'S' => "####..###..####",
        'T' => "###.#..#..#..#.",
        'U' => "#.##.##.##.####",
        'V' => "#.##.##.##.#.#.",
        'W' => "#.##.########.#",
        'X' => "#.##.#.#.#.##.#",
        'Y' => "#.##.#.#..#..#.",
        'Z' => "###..#.#.#..###",
        '0' => "####.##.##.####",
        '1' => ".#.##..#..#.###",
        '2' => "###..#####..###",
        '3' => "###..####..####",
        '4' => "#.##.####..#..#",
        '5' => "####..###..####",
        '6' => "####..####.####",
        '7' => "###..#..#.#..#.",
        '8' => "####.#####.####",
        '9' => "####.####..####",
        '.' => ".............#.",
        ',' => "..........#.#..",
        '-' => "......###......",
        '+' => "....#.###.#....",
        '/' => "..#..#.#.#..#..",
        '(' => ".#.#..#..#...#.",
        ')' => ".#...#..#..#.#.",
        '%' => "#.#..#.#.#..#.#",
        ':' => "....#.....#....",
        '=' => "...###...###...",
        '<' => "..#.#.#...#...#",
        '>' => "#...#...#.#.#..",
        '_' => "............###",
        _ => return None,
    })
}
