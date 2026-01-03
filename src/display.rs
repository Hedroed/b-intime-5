use esp_hal::{spi::master::Spi, Blocking};

use crate::font::{Font, ALPHABET_BIG_DIGITS, ALPHABET_NANO, ALPHABET_NORMAL, ALPHABET_TINY};

#[derive(Clone, Copy)]
pub enum Command {
    Noop = 0x00,
    Digit0 = 0x01,
    Digit1 = 0x02,
    Digit2 = 0x03,
    Digit3 = 0x04,
    Digit4 = 0x05,
    Digit5 = 0x06,
    Digit6 = 0x07,
    Digit7 = 0x08,
    DecodeMode = 0x09,
    Intensity = 0x0A,
    ScanLimit = 0x0B,
    Power = 0x0C,
    DisplayTest = 0x0F,
}

pub static COMMAND_DIGITS: [Command; 8] = [
    Command::Digit0,
    Command::Digit1,
    Command::Digit2,
    Command::Digit3,
    Command::Digit4,
    Command::Digit5,
    Command::Digit6,
    Command::Digit7,
];

pub static NOOP: Order = Order {
    command: Command::Noop,
    data: 0,
};

impl Default for Command {
    fn default() -> Self {
        Command::Noop
    }
}

#[derive(Clone, Copy, Default)]
pub struct Order {
    pub command: Command,
    pub data: u8,
}

pub fn order(command: Command, data: u8) -> Order {
    Order { command, data }
}

pub struct Canvas<const W: usize, const H: usize>(pub [[bool; H]; W]);

impl<const W: usize, const H: usize> Canvas<W, H> {
    pub fn init() -> Self {
        Canvas([[false; H]; W])
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, val: bool) {
        if x >= W || y >= H {
            return;
        }
        self.0[x][y] = val;
    }

    pub fn on(&mut self, x: usize, y: usize) {
        self.set_pixel(x, y, true);
    }
    pub fn off(&mut self, x: usize, y: usize) {
        self.set_pixel(x, y, false);
    }

    fn print_line8(&mut self, x: usize, y: usize, line: u8) {
        if x >= W || y >= H {
            return;
        }

        for idx_bits in 0..(W - x).min(8) {
            if line >> (7 - idx_bits) & 0b1 == 1 {
                self.0[x + idx_bits][y] = true;
            } else {
                self.0[x + idx_bits][y] = false;
            }
        }
    }

    fn print_font<const N: usize>(&mut self, font: Font<N>, x: usize, y: usize, text: &str) {
        let mut cursor = x;
        for letter in text.chars() {
            for row in 0..font.height {
                let code = font.to_line(row, letter);
                self.print_line8(cursor, y + row, code);
            }
            cursor += font.width_of(letter) as usize;
        }
    }

    pub fn print_8x8(&mut self, x: usize, y: usize, text: &str) {
        self.print_font(ALPHABET_BIG_DIGITS, x, y, text);
    }

    pub fn print_5x7(&mut self, x: usize, y: usize, text: &str) {
        self.print_font(ALPHABET_NORMAL, x, y, text);
    }

    pub fn print_4x6(&mut self, x: usize, y: usize, text: &str) {
        self.print_font(ALPHABET_TINY, x, y, text);
    }

    pub fn print_4x4(&mut self, x: usize, y: usize, text: &str) {
        self.print_font(ALPHABET_NANO, x, y, text);
    }

    pub fn to_raw<const T: usize>(&self) -> [[u8; T]; 8] {
        let mut buf = [[0u8; T]; 8];
        // for y in 0..H {
        //     for x in 0..W {
        //         print!("{}", self.0[x][y] as u8);
        //     }
        //     print!("\n");
        // }
        for x in 0..(W / 8) {
            for y in 0..H {
                let fy = if y >= 8 { y - 8 } else { y };
                let fx = if y >= 8 { x + 4 } else { x };

                for idx in 0..8 {
                    if self.0[(x * 8) + idx][y] {
                        buf[fy][fx] |= 0b1 << (7 - idx);
                    }
                }
            }
        }
        // for y in 0..H {
        //     if y < 8 {
        //         info!("{:08b} {:08b} {:08b} {:08b}", buf[y][0], buf[y][1], buf[y][2], buf[y][3]);
        //     } else {
        //         info!("{:08b} {:08b} {:08b} {:08b}", buf[y-8][4], buf[y-8][5], buf[y-8][6], buf[y-8][7]);
        //     };
        // }
        buf
    }
}

pub struct Screen<const N: usize> {}

const MAX_DISPLAYS_COUNT: usize = 16;

impl<const N: usize> Screen<N> {
    pub fn init(spi: &mut Spi<'_, Blocking>) {
        if N > MAX_DISPLAYS_COUNT {
            panic!("too many displays {N}");
        }

        Screen::<N>::send_all(spi, order(Command::DisplayTest, 0));
        Screen::<N>::send_all(spi, order(Command::ScanLimit, 0x07));
        Screen::<N>::send_all(spi, order(Command::DecodeMode, 0));

        for cmd in COMMAND_DIGITS {
            Screen::<N>::send_all(spi, order(cmd, 0));
        }

        Screen::<N>::send_all(spi, order(Command::Intensity, 0));
        Screen::<N>::send_all(spi, order(Command::Power, 1));
    }

    pub fn send_all(spi: &mut Spi<'_, Blocking>, order: Order) {
        if N > MAX_DISPLAYS_COUNT {
            panic!("too many displays {N}");
        }

        let mut buf = [0u8; 2 * MAX_DISPLAYS_COUNT];
        for idx_data in 0..N {
            let idx = idx_data * 2;
            buf[idx] = order.command as u8;
            buf[idx + 1] = order.data;
        }
        spi.write(&buf[0..(2 * N)]).expect("spi write fail");
    }

    pub fn send(spi: &mut Spi<'_, Blocking>, command: Command, data: &[u8; N]) {
        if N > MAX_DISPLAYS_COUNT {
            panic!("too many displays {N}");
        }

        let mut buf = [0u8; 2 * MAX_DISPLAYS_COUNT];
        for (idx_data, val) in data.iter().enumerate() {
            let idx = idx_data * 2;
            buf[idx] = command as u8;
            buf[idx + 1] = val.clone();
        }
        spi.write(&buf[0..(2 * N)]).expect("spi write fail");
    }

    pub fn draw<const W: usize, const H: usize>(
        spi: &mut Spi<'_, Blocking>,
        canvas: &Canvas<W, H>,
    ) {
        let raw = canvas.to_raw::<N>();
        for (idx_digit, cmd) in COMMAND_DIGITS.iter().enumerate() {
            Screen::send(spi, cmd.clone(), &raw[idx_digit]);
        }
    }
}
