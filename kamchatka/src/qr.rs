//! A QR code of a short text, drawn with the terminal's own characters.
//!
//! note: for one thing, the page's address, so that a phone on the same network can open it by
//! pointing a camera at the terminal rather than typing a key it was never meant to remember. Byte
//! mode, error correction level `M`, versions 1 to 6: 106 bytes at most, which holds any address
//! `--web` can listen on with room to spare, and stops short of the versions that need a block of
//! version information - the one part of the format this would otherwise have to carry for nothing.
//!
//! note: hand-rolled rather than a dependency, because what is needed is a few hundred lines of
//! arithmetic that does not change, and the format is fixed by ISO/IEC 18004. The tests hold it to
//! the standard's worked example and to a whole code as a reference encoder draws it, so a mistake
//! here is a test failing rather than a phone that shrugs.

/// The data codewords in each block and the error correction codewords per block, by version, at
/// level `M`.
const BLOCKS: [(usize, usize, usize); 6] = [
    // (blocks, data codewords per block, error correction codewords per block)
    (1, 16, 10),
    (1, 28, 16),
    (1, 44, 26),
    (2, 32, 18),
    (2, 43, 24),
    (4, 27, 16),
];

/// A QR code: a square of modules, each dark or light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    size: usize,
    /// Row by row; `true` is dark.
    dark: Vec<bool>,
}

impl Code {
    /// The code for `text`, or `None` where it is longer than the 106 bytes this draws.
    pub fn new(text: &str) -> Option<Self> {
        Self::masked(text, None)
    }

    /// The code with the given mask, or with the best one where it is `None`.
    fn masked(text: &str, mask: Option<usize>) -> Option<Self> {
        let text = text.as_bytes();
        let version = (1..=BLOCKS.len()).find(|&v| {
            let (blocks, data, _) = BLOCKS[v - 1];
            // the mode and the length take 12 bits, which is two of the codewords
            text.len() + 2 <= blocks * data
        })?;
        let codewords = codewords(text, version);

        let mut code = Grid::new(version);
        code.draw_functions();
        code.draw_data(&codewords);
        // every mask is a valid code; the one with the fewest look-alikes of the finders and the
        // fewest long runs is the one a camera reads most easily
        let mask = mask.unwrap_or_else(|| {
            (0..8)
                .min_by_key(|&mask| {
                    let mut tried = code.clone();
                    tried.apply_mask(mask);
                    tried.draw_format(mask);
                    tried.penalty()
                })
                .unwrap_or_default()
        });
        code.apply_mask(mask);
        code.draw_format(mask);

        Some(Self {
            size: code.size,
            dark: code.dark,
        })
    }

    /// The number of modules along a side, without the quiet zone.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Whether the module at column `x` and row `y` is dark; outside the code is light.
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.dark[y * self.size + x]
    }

    /// The code as lines of text, two rows of modules to a line, inside a quiet zone of two.
    ///
    /// note: a *light* module is the one drawn with a block, because a terminal draws its text
    /// light on dark far more often than not, and a code the wrong way round is one some cameras
    /// will not read. Where the colours can be set, a block drawn white on black is right on any
    /// background.
    ///
    /// note: half blocks, so that a module is about as tall as it is wide: a terminal cell is
    /// roughly twice as tall as it is wide, and a code of whole cells is stretched to double
    /// height and takes twice the lines.
    pub fn lines(&self) -> Vec<String> {
        const QUIET: usize = 2;
        let side = self.size + 2 * QUIET;
        let light =
            |x: usize, y: usize| !(x >= QUIET && y >= QUIET && self.is_dark(x - QUIET, y - QUIET));

        (0..side)
            .step_by(2)
            .map(|y| {
                (0..side)
                    .map(|x| match (light(x, y), y + 1 < side && light(x, y + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    })
                    .collect()
            })
            .collect()
    }

    /// Whether a line is one of [`Code::lines`]: made of nothing but blocks and spaces, with at
    /// least one block in it.
    pub fn is_line(line: &str) -> bool {
        line.chars().any(|c| matches!(c, '█' | '▀' | '▄'))
            && line.chars().all(|c| matches!(c, '█' | '▀' | '▄' | ' '))
    }
}

/// The modules being drawn, and which of them are the format's own rather than data.
#[derive(Clone)]
struct Grid {
    version: usize,
    size: usize,
    dark: Vec<bool>,
    function: Vec<bool>,
}

impl Grid {
    fn new(version: usize) -> Self {
        let size = 17 + 4 * version;
        Self {
            version,
            size,
            dark: vec![false; size * size],
            function: vec![false; size * size],
        }
    }

    fn set(&mut self, x: usize, y: usize, dark: bool) {
        self.dark[y * self.size + x] = dark;
        self.function[y * self.size + x] = true;
    }

    /// The finders, the timing lines, the alignment pattern, and the format's place kept.
    fn draw_functions(&mut self) {
        let size = self.size;
        for i in 0..size {
            self.set(6, i, i % 2 == 0);
            self.set(i, 6, i % 2 == 0);
        }
        for (x, y) in [(3, 3), (size - 4, 3), (3, size - 4)] {
            for dy in -4isize..=4 {
                for dx in -4isize..=4 {
                    let (xx, yy) = (x as isize + dx, y as isize + dy);
                    if (0..size as isize).contains(&xx) && (0..size as isize).contains(&yy) {
                        let ring = dx.abs().max(dy.abs());
                        self.set(xx as usize, yy as usize, ring != 2 && ring != 4);
                    }
                }
            }
        }
        // versions 2 to 6 have one, in the corner the finders leave
        if self.version > 1 {
            let at = size - 7;
            for dy in -2isize..=2 {
                for dx in -2isize..=2 {
                    let ring = dx.abs().max(dy.abs());
                    self.set(
                        (at as isize + dx) as usize,
                        (at as isize + dy) as usize,
                        ring != 1,
                    );
                }
            }
        }
        // kept now and drawn once the mask is chosen
        self.draw_format(0);
    }

    /// The level and the mask, twice over, and the module that is always dark.
    fn draw_format(&mut self, mask: usize) {
        // level `M` is `00`
        let data = mask;
        let mut remainder = data;
        for _ in 0..10 {
            remainder = (remainder << 1) ^ ((remainder >> 9) * 0x537);
        }
        let bits = ((data << 10) | remainder) ^ 0x5412;
        let bit = |i: usize| (bits >> i) & 1 == 1;

        let size = self.size;
        for i in 0..=5 {
            self.set(8, i, bit(i));
        }
        self.set(8, 7, bit(6));
        self.set(8, 8, bit(7));
        self.set(7, 8, bit(8));
        for i in 9..15 {
            self.set(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.set(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set(8, size - 15 + i, bit(i));
        }
        self.set(8, size - 8, true);
    }

    /// The codewords, in two-column strips from the bottom right, up and down in turn.
    fn draw_data(&mut self, codewords: &[u8]) {
        let size = self.size;
        let mut i = 0;
        let mut right = size - 1;
        loop {
            if right == 6 {
                right = 5;
            }
            let upward = (right + 1) & 2 == 0;
            for vert in 0..size {
                for j in 0..2 {
                    let x = right - j;
                    let y = if upward { size - 1 - vert } else { vert };
                    if !self.function[y * size + x] && i < codewords.len() * 8 {
                        self.dark[y * size + x] = (codewords[i >> 3] >> (7 - (i & 7))) & 1 == 1;
                        i += 1;
                    }
                }
            }
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: usize) {
        for y in 0..self.size {
            for x in 0..self.size {
                let flip = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                let at = y * self.size + x;
                if flip && !self.function[at] {
                    self.dark[at] = !self.dark[at];
                }
            }
        }
    }

    /// How hard the code is to read, by the four rules the standard scores a mask by.
    fn penalty(&self) -> usize {
        let size = self.size;
        let at = |x: usize, y: usize| self.dark[y * size + x];
        let mut score = 0;

        // runs of five or more of one colour, and the finder's 1:1:3:1:1 with four light beside it
        const FINDER: [bool; 7] = [true, false, true, true, true, false, true];
        for horizontal in [true, false] {
            for a in 0..size {
                let line: Vec<bool> = (0..size)
                    .map(|b| if horizontal { at(b, a) } else { at(a, b) })
                    .collect();
                let mut run = 1;
                for b in 1..=size {
                    if b < size && line[b] == line[b - 1] {
                        run += 1;
                        continue;
                    }
                    if run >= 5 {
                        score += run - 2;
                    }
                    run = 1;
                }
                for start in 0..=size.saturating_sub(7) {
                    if line[start..start + 7] != FINDER {
                        continue;
                    }
                    let light = |from: isize, to: isize| {
                        (from..to).all(|i| i < 0 || i >= size as isize || !line[i as usize])
                    };
                    let start = start as isize;
                    if light(start - 4, start) || light(start + 7, start + 11) {
                        score += 40;
                    }
                }
            }
        }
        // two by two of one colour
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let c = at(x, y);
                if c == at(x + 1, y) && c == at(x, y + 1) && c == at(x + 1, y + 1) {
                    score += 3;
                }
            }
        }
        // how far from half dark, in steps of five percent
        let dark = self.dark.iter().filter(|it| **it).count();
        let total = size * size;
        score += (dark * 20).abs_diff(total * 10) / total * 10;

        score
    }
}

/// The text in byte mode, padded, split into blocks, given error correction and interleaved.
fn codewords(text: &[u8], version: usize) -> Vec<u8> {
    let (blocks, per_block, ec) = BLOCKS[version - 1];
    let capacity = blocks * per_block;

    let mut bits: Vec<bool> = Vec::with_capacity(capacity * 8);
    let mut push = |value: usize, width: usize| {
        for i in (0..width).rev() {
            bits.push((value >> i) & 1 == 1);
        }
    };
    push(0b0100, 4);
    push(text.len(), 8);
    for &byte in text {
        push(byte.into(), 8);
    }
    let terminator = (capacity * 8 - bits.len()).min(4);
    bits.extend(std::iter::repeat_n(false, terminator));
    bits.resize(bits.len().div_ceil(8) * 8, false);
    let mut data: Vec<u8> = bits
        .chunks(8)
        .map(|byte| byte.iter().fold(0, |acc, &bit| (acc << 1) | u8::from(bit)))
        .collect();
    for pad in [0xEC, 0x11].into_iter().cycle() {
        if data.len() >= capacity {
            break;
        }
        data.push(pad);
    }

    let divisor = generator(ec);
    let split: Vec<&[u8]> = data.chunks(per_block).collect();
    let corrections: Vec<Vec<u8>> = split
        .iter()
        .map(|block| remainder(block, &divisor))
        .collect();
    let mut out = Vec::with_capacity(blocks * (per_block + ec));
    for i in 0..per_block {
        out.extend(split.iter().map(|block| block[i]));
    }
    for i in 0..ec {
        out.extend(corrections.iter().map(|block| block[i]));
    }

    out
}

/// Multiplies in GF(2^8), reduced by the code's polynomial, `x^8 + x^4 + x^3 + x^2 + 1`.
fn multiply(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 == 1 {
            product ^= a;
        }
        let carry = a & 0x80 != 0;
        a <<= 1;
        if carry {
            a ^= 0x1D;
        }
        b >>= 1;
    }

    product
}

/// The Reed-Solomon generator of the given degree, its leading `1` left out, highest power first.
fn generator(degree: usize) -> Vec<u8> {
    let mut poly = vec![0u8; degree];
    poly[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            poly[j] = multiply(poly[j], root);
            if j + 1 < degree {
                poly[j] ^= poly[j + 1];
            }
        }
        root = multiply(root, 2);
    }

    poly
}

/// The error correction codewords for one block.
fn remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; divisor.len()];
    for &byte in data {
        let factor = byte ^ out[0];
        out.rotate_left(1);
        let last = out.len() - 1;
        out[last] = 0;
        for (slot, &coefficient) in out.iter_mut().zip(divisor) {
            *slot ^= multiply(coefficient, factor);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole code, module for module, as a reference encoder draws it with the same mask.
    ///
    /// note: version 2, so that the alignment pattern is in it. Every mask at every version this
    /// draws was held to the same encoder when this was written, and the codes it picked were
    /// read back by a decoder; this is the one kept, so that a change to the placement, the
    /// interleaving or the masks has something to disagree with.
    #[test]
    fn a_code_is_drawn_as_the_standard_draws_it() {
        let code = Code::masked("http://10.0.0.1:80/ab/", Some(5)).expect("it fits");
        let drawn: Vec<String> = (0..code.size())
            .map(|y| {
                (0..code.size())
                    .map(|x| match code.is_dark(x, y) {
                        true => '#',
                        false => '.',
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            drawn,
            [
                "#######..##..#..#.#######",
                "#.....#.###.#.....#.....#",
                "#.###.#.#...#..#..#.###.#",
                "#.###.#.#.######..#.###.#",
                "#.###.#.....##....#.###.#",
                "#.....#..#..###...#.....#",
                "#######.#.#.#.#.#.#######",
                "........##.##.##.........",
                "#.....#.##..###.###..###.",
                "##..##..#.##..###...####.",
                "#..#.######....###...#.##",
                "##.#.#.#.####.#......#..#",
                "#..##.#####....#.##.....#",
                "##..##.###..#..#.#.....#.",
                "#...####..#..#.#..##.#.##",
                "#.#.##....#.#.##..#.#.#.#",
                "#.###.##..#.##..#####.#..",
                "........#.#..#..#...#.#..",
                "#######..##.#...#.#.##..#",
                "#.....#....#..#.#...#....",
                "#.###.#..###...##########",
                "#.###.#..##.###.#.##.#.##",
                "#.###.#..#...###......#.#",
                "#.....#..#..##.###.##...#",
                "#######.##.###.###...#..#",
            ]
        );
    }

    /// The worked example in ISO/IEC 18004 Annex I: `01234567` at level `M` is version 1, and
    /// its ten error correction codewords are fixed.
    ///
    /// note: that example is numeric mode, and this draws byte mode only, so what it checks is
    /// the arithmetic underneath - the field, the generator and the division - on the standard's
    /// own data codewords.
    #[test]
    fn the_error_correction_is_the_standards() {
        let data = [
            0x10, 0x20, 0x0C, 0x56, 0x61, 0x80, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11,
            0xEC, 0x11,
        ];
        assert_eq!(
            remainder(&data, &generator(10)),
            [0xA5, 0x24, 0xD4, 0xC1, 0xED, 0x36, 0xC7, 0x87, 0x2C, 0x55]
        );
    }

    /// The format bits for level `M` with mask 0 are the standard's `101010000010010`.
    #[test]
    fn the_format_is_the_standards() {
        let mut grid = Grid::new(1);
        grid.draw_format(0);
        let read: String = (0..=5)
            .map(|y| (8, y))
            .chain([(8, 7), (8, 8), (7, 8)])
            .chain((9..15).map(|i| (14 - i, 8)))
            .map(|(x, y)| match grid.dark[y * grid.size + x] {
                true => '1',
                false => '0',
            })
            .rev()
            .collect();
        assert_eq!(read, "101010000010010");
    }

    /// A text is drawn at the smallest version that holds it, and one too long is refused.
    #[test]
    fn the_version_is_the_smallest_that_holds_it() {
        assert_eq!(Code::new(&"a".repeat(14)).map(|it| it.size()), Some(21));
        assert_eq!(Code::new(&"a".repeat(15)).map(|it| it.size()), Some(25));
        assert_eq!(Code::new(&"a".repeat(106)).map(|it| it.size()), Some(41));
        assert_eq!(Code::new(&"a".repeat(107)), None);
    }

    /// Two rows to a line, the quiet zone light all round, and every line one the overlay knows.
    #[test]
    fn the_lines_are_half_as_many_as_the_rows() {
        let code = Code::new("http://192.168.1.5:8080/0123abcd/").expect("it fits");
        let lines = code.lines();
        let side = code.size() + 4;
        assert_eq!(lines.len(), side.div_ceil(2));
        assert!(lines.iter().all(|line| line.chars().count() == side));
        assert!(lines[0].chars().all(|c| c == '█'), "{}", lines[0]);
        assert!(lines.iter().all(|line| Code::is_line(line)));
        assert!(!Code::is_line("   "));
        assert!(!Code::is_line("█ a"));
    }
}
