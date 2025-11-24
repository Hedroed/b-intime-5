
const fn build_glyph(width: u8, val: u64) -> Glyph {
    return Glyph {
        width,
        data: val.to_be_bytes(),
    };
}

pub struct Glyph {
    pub width: u8,
    data: [u8; (u64::BITS/8) as usize],
}

pub struct Font<const N: usize> {
    glyphs: [Glyph; N],
    lower: u8,
    higher: u8,
    fallback: char,
    pub height: usize,
}

impl<const N: usize> Font<N> {
    pub const fn init(
        height: usize,
        lower: char,
        higher: char,
        fallback: char,
        glyphs: [Glyph; N],
    ) -> Option<Self> {
        let lower_u8 = lower as u8;
        let higher_u8 = higher as u8;
        if ((higher_u8 - lower_u8) as usize) < N {
            Some(Font {
                glyphs,
                lower: lower_u8,
                higher: higher_u8,
                fallback,
                height,
            })
        } else {
            None
        }
    }

    pub fn width_of_unchecked(&self, val: char) -> u8 {
        let idx = val as u8 - self.lower;
        return self.glyphs[idx as usize].width;
    }

    pub fn width_of(&self, val: char) -> u8 {
        if (val as u8) < self.lower || (val as u8) > self.higher {
            self.width_of_unchecked(self.fallback)
        } else {
            self.width_of_unchecked(val)
        }
    }
    
    pub fn to_line_unchecked(&self, position: usize, val: char) -> u8 {
        let idx = val as u8 - self.lower;
        return self.glyphs[idx as usize].data[position]
    }

    pub fn to_line(&self, position: usize, val: char) -> u8 {
        if (val as u8) < self.lower || (val as u8) > self.higher {
            self.to_line_unchecked(position, self.fallback)
        } else {
            self.to_line_unchecked(position, val)
        }
    }
}

pub const ALPHABET_BIG_DIGITS: Font<11> = Font::init(
    8,
    '0',
    ':',
    ':',
    [
        build_glyph(7, 0x384c5c6c4c4c3800), // 0
        build_glyph(7, 0x1818381818183c00), // 1
        build_glyph(7, 0x384c0c1820407c00), // 2
        build_glyph(7, 0x384c0c380c4c3800), // 3
        build_glyph(7, 0x0c1c3c4c7e0c0c00), // 4
        build_glyph(7, 0x7c40780c0c4c3800), // 5
        build_glyph(7, 0x384c40784c4c3800), // 6
        build_glyph(7, 0x7c4c183030303000), // 7
        build_glyph(7, 0x384c4c384c4c3800), // 8
        build_glyph(7, 0x384c4c3c0c4c3800), // 9
        build_glyph(3, 0x0000400040000000), // :
    ],
)
.expect("ALPHABET_BIG_DIGITS");

pub const ALPHABET_NORMAL: Font<95> = Font::init(
    7,
    ' ',
    '~',
    '?',
    [
        build_glyph(5, 0x0000000000000000), //
        build_glyph(4, 0x2020202000200000), // !
        build_glyph(5, 0x5050500000000000), // "
        build_glyph(5, 0x0050f850f8500000), // #
        build_glyph(5, 0x0070a07028700000), // $
        build_glyph(5, 0x8090204090100000), // %
        build_glyph(6, 0xc0c0182020180000), // &
        build_glyph(5, 0x8080800000000000), // '
        build_glyph(5, 0x2040404040200000), // (
        build_glyph(5, 0x4020202020400000), // )
        build_glyph(5, 0x40a0400000000000), // *
        build_glyph(5, 0x002020f820200000), // +
        build_glyph(5, 0x0000000030204000), // ,
        build_glyph(5, 0x0000007000000000), // -
        build_glyph(5, 0x0000000060600000), // .
        build_glyph(5, 0x0010204080000000), // /
        build_glyph(5, 0x6090909090600000), // 0
        build_glyph(5, 0x2060202020700000), // 1
        build_glyph(5, 0x6090102040f00000), // 2
        build_glyph(5, 0xf010601090600000), // 3
        build_glyph(5, 0x2060a0f020200000), // 4
        build_glyph(5, 0xf080e01090600000), // 5
        build_glyph(5, 0x6080e09090600000), // 6
        build_glyph(5, 0xf010202040400000), // 7
        build_glyph(5, 0x6090609090600000), // 8
        build_glyph(5, 0x6090907010600000), // 9
        build_glyph(5, 0x0060600060600000), // :
        build_glyph(5, 0x0060600060408000), // ;
        build_glyph(5, 0x0010204020100000), // <
        build_glyph(5, 0x0000f000f0000000), // =
        build_glyph(5, 0x0040201020400000), // >
        build_glyph(5, 0x2050102000200000), // ?
        build_glyph(5, 0x6090b0b080600000), // @
        build_glyph(5, 0x609090f090900000), // A
        build_glyph(5, 0xe090e09090e00000), // B
        build_glyph(5, 0x6090808090600000), // C
        build_glyph(5, 0xe090909090e00000), // D
        build_glyph(5, 0xf080e08080f00000), // E
        build_glyph(5, 0xf080e08080800000), // F
        build_glyph(5, 0x609080b090700000), // G
        build_glyph(5, 0x9090f09090900000), // H
        build_glyph(5, 0x7020202020700000), // I
        build_glyph(5, 0x1010101090600000), // J
        build_glyph(5, 0x90a0c0c0a0900000), // K
        build_glyph(5, 0x8080808080f00000), // L
        build_glyph(5, 0x90f0f09090900000), // M
        build_glyph(5, 0x90d0d0b0b0900000), // N
        build_glyph(5, 0x6090909090600000), // O
        build_glyph(5, 0xe09090e080800000), // P
        build_glyph(5, 0x60909090d0601000), // Q
        build_glyph(5, 0xe09090e0a0900000), // R
        build_glyph(5, 0x6090402090600000), // S
        build_glyph(5, 0x7020202020200000), // T
        build_glyph(5, 0x9090909090600000), // U
        build_glyph(5, 0x9090909060600000), // V
        build_glyph(5, 0x909090f0f0900000), // W
        build_glyph(5, 0x9090606090900000), // X
        build_glyph(5, 0x5050502020200000), // Y
        build_glyph(5, 0xf010204080f00000), // Z
        build_glyph(5, 0x7040404040700000), // [
        build_glyph(5, 0x0080402010000000), // "\"
        build_glyph(5, 0x7010101010700000), // ]
        build_glyph(5, 0x2050000000000000), // ^
        build_glyph(5, 0x0000000000f00000), // _
        build_glyph(5, 0x4020000000000000), // `
        build_glyph(5, 0x00007090b0500000), // a
        build_glyph(5, 0x8080e09090e00000), // b
        build_glyph(5, 0x0000608080600000), // c
        build_glyph(5, 0x1010709090700000), // d
        build_glyph(5, 0x000060b0c0600000), // e
        build_glyph(5, 0x205040e040400000), // f
        build_glyph(5, 0x0000709060807000), // g
        build_glyph(5, 0x8080e09090900000), // h
        build_glyph(5, 0x2000602020700000), // i
        build_glyph(5, 0x1000101010502000), // j
        build_glyph(5, 0x8080a0c0a0900000), // k
        build_glyph(5, 0x6020202020700000), // l
        build_glyph(5, 0x0000a0f090900000), // m
        build_glyph(5, 0x0000e09090900000), // n
        build_glyph(5, 0x0000609090600000), // o
        build_glyph(5, 0x0000e09090e08000), // p
        build_glyph(5, 0x0000709090701000), // q
        build_glyph(5, 0x0000e09080800000), // r
        build_glyph(5, 0x000070c030e00000), // s
        build_glyph(5, 0x4040e04040300000), // t
        build_glyph(5, 0x0000909090700000), // u
        build_glyph(5, 0x0000505050200000), // v
        build_glyph(5, 0x00009090f0f00000), // w
        build_glyph(5, 0x0000906060900000), // x
        build_glyph(5, 0x0000909050204000), // y
        build_glyph(5, 0x0000f02040f00000), // z
        build_glyph(5, 0x1020602020100000), // {
        build_glyph(5, 0x2020202020200000), // |
        build_glyph(5, 0x4020302020400000), // }
        build_glyph(5, 0x50a0000000000000), // ~
    ],
)
.expect("ALPHABET_NORMAL");

pub const ALPHABET_TINY: Font<16> = Font::init(
    6,
    '0',
    '?',
    '?',
    [
        build_glyph(4, 0xe0a0a0a0e0000000), // 0
        build_glyph(4, 0x2020202020000000), // 1
        build_glyph(4, 0xe020e080e0000000), // 2
        build_glyph(4, 0xe020e020e0000000), // 3
        build_glyph(4, 0xa0a0e02020000000), // 4
        build_glyph(4, 0xe080e020e0000000), // 5
        build_glyph(4, 0xe080e0a0e0000000), // 6
        build_glyph(4, 0xe020202020000000), // 7
        build_glyph(4, 0xe0a0e0a0e0000000), // 8
        build_glyph(4, 0xe0a0e020e0000000), // 9
        build_glyph(4, 0x0040004000000000), // :
        build_glyph(4, 0x0000400040800000), // ;
        build_glyph(4, 0x2040804020000000), // <
        build_glyph(4, 0x00e000e000000000), // =
        build_glyph(4, 0x8040204080000000), // >
        build_glyph(4, 0x40a0204040000000), // ?
    ],
)
.expect("ALPHABET_TINY");

pub const ALPHABET_NANO: Font<16> = Font::init(
    4,
    '0',
    '?',
    '?',
    [
        build_glyph(4, 0xe0a0a0e000000000), // 0
        build_glyph(4, 0x2020202000000000), // 1
        build_glyph(4, 0xe020c0e000000000), // 2
        build_glyph(4, 0xe06020e000000000), // 3
        build_glyph(4, 0xa0a0e02000000000), // 4
        build_glyph(4, 0xe0c020e000000000), // 5
        build_glyph(4, 0xe080e0e000000000), // 6
        build_glyph(4, 0xe020602000000000), // 7
        build_glyph(4, 0xe0e0a0e000000000), // 8
        build_glyph(4, 0xe0e020e000000000), // 9
        build_glyph(3, 0x4000400000000000), // :
        build_glyph(3, 0x4000406000000000), // ;
        build_glyph(3, 0x0060806000000000), // <
        build_glyph(4, 0xe000e00000000000), // =
        build_glyph(3, 0xc020c00000000000), // >
        build_glyph(4, 0xe020004000000000), // ?
    ],
)
.expect("ALPHABET_NANO");
