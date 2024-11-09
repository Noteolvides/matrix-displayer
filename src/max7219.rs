use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::{Dimensions, DrawTarget, OriginDimensions, PointsIter, Size},
    Pixel,
};
use embedded_hal::blocking::spi::Write;

const MAX_DIGITS: usize = 8;

#[derive(Clone, Copy)]
pub enum Command {
    DecodeMode = 0x09,
    Intensity = 0x0A,
    ScanLimit = 0x0B,
    Power = 0x0C,
    DisplayTest = 0x0F,
}

/// Decode modes for BCD encoded input.
#[derive(Copy, Clone)]
pub enum DecodeMode {
    NoDecode = 0x00,
}

pub struct Max7219<SPI, const ROWS: usize, const COLS: usize>
where
    SPI: Write<u8>,
    [u8; ROWS*COLS * 2]: Sized,
{
    devices: usize,
    spi: SPI,
    buffer: [[u8; MAX_DIGITS]; ROWS*COLS],
}

impl<SPI, const ROWS: usize, const COLS: usize> Max7219<SPI, ROWS, COLS>
where
    SPI: Write<u8>,
    [u8; ROWS*COLS * 2]: Sized,
{
    pub fn new(spi: SPI) -> Self {
        Max7219 {
            devices: ROWS * COLS,
            spi,
            buffer: [[0; 8]; ROWS * COLS],
        }
    }

    pub fn init(&mut self) -> Result<(), SPI::Error> {
        for i in 0..self.devices {
            self.test(i, false)?; // turn testmode off
            self.write_data(i, Command::ScanLimit, 0x07)?; // set scanlimit
            self.set_intensity(i,1)?;
            self.set_decode_mode(i, DecodeMode::NoDecode)?; // direct decode
            self.clear_display(i)?; // clear all digits
        }
        self.power_off()?; // power off

        Ok(())
    }

    pub fn test(&mut self, addr: usize, is_on: bool) -> Result<(), SPI::Error> {
        if is_on {
            self.write_data(addr, Command::DisplayTest, 0x01)
        } else {
            self.write_data(addr, Command::DisplayTest, 0x00)
        }
    }

    pub fn set_decode_mode(&mut self, addr: usize, mode: DecodeMode) -> Result<(), SPI::Error> {
        self.write_data(addr, Command::DecodeMode, mode as u8)
    }

    fn write_data(&mut self, addr: usize, command: Command, data: u8) -> Result<(), SPI::Error> {
        self.write_raw(addr, command as u8, data)
    }

    fn write_raw(&mut self, addr: usize, header: u8, data: u8) -> Result<(), SPI::Error> {
        let offset = addr * 2;
        let max_bytes = self.devices * 2;
        let mut buffer = [0; ROWS * COLS * 2];

        buffer[offset] = header;
        buffer[offset + 1] = data;

        self.spi.write(&buffer[0..max_bytes])?;

        Ok(())
    }

    pub fn power_on(&mut self) -> Result<(), SPI::Error> {
        for i in 0..self.devices {
            self.write_data(i, Command::Power, 0x01)?;
        }

        Ok(())
    }

    pub fn power_off(&mut self) -> Result<(), SPI::Error> {
        for i in 0..self.devices {
            self.write_data(i, Command::Power, 0x00)?;
        }

        Ok(())
    }

    pub fn clear_display(&mut self, addr: usize) -> Result<(), SPI::Error> {
        for i in 1..9 {
            self.write_raw(addr, i, 0x00)?;
        }

        Ok(())
    }

    pub fn set_intensity(&mut self, addr: usize, intensity: u8) -> Result<(), SPI::Error> {
        self.write_data(addr, Command::Intensity, intensity)
    }

    #[allow(dead_code)]
    pub fn write_display(&mut self, addr: usize, raw: &[u8; MAX_DIGITS]) -> Result<(), SPI::Error> {
        self.set_decode_mode(0, DecodeMode::NoDecode)?;

        let mut digit: u8 = 1;
        for b in raw {
            self.write_raw(addr, digit, *b)?;
            digit += 1;
        }

        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), SPI::Error> {
        for digit in 0..8 {
            // Buffer to hold the SPI data for all displays
            let mut spi_buffer = [0u8; ROWS * COLS * 2];

            // Fill the buffer with the data for each display
            for display in 0..(ROWS * COLS) {
                // Each display gets two bytes: [register, data]
                spi_buffer[display * 2] = digit as u8 + 1; // Register (1-based digit index)
                spi_buffer[display * 2 + 1] = self.buffer[display][digit]; // Data for that digit
            }

            // Send the entire SPI buffer to all displays in the chain
            self.spi.write(&spi_buffer)?;
        }
        Ok(())
    }

    /// Maps an (x, y) pixel coordinate to the corresponding display and pixel position within that display
    /// considering a snake pattern display arrangement.
    fn map_coordinates(&self, x: usize, y: usize) -> Option<(usize, usize, usize)> {
        // Determine the column and row of the display in the grid
        let display_col = x / 8; // Each display is 8 pixels wide
        let display_row = y / 8; // Each display is 8 pixels high

        // Calculate the display index based on the desired pattern
        let display_idx = (ROWS - 1 - display_row) * COLS + display_col;
        
        // Calculate the local pixel within the display
        let local_x = x % 8;
        let local_y = y % 8;
        Some((display_idx, local_x, local_y))
    }
}

impl<SPI, const ROWS: usize, const COLS: usize> DrawTarget for Max7219<SPI, ROWS, COLS>
where
    SPI: Write<u8>,
    [u8; ROWS*COLS * 2]: Sized,
{
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<BinaryColor>>,
    {
        let bb = self.bounding_box();

        pixels
            .into_iter()
            .filter(|Pixel(pos, _color)| bb.contains(*pos))
            .for_each(|Pixel(pos, color)| {
                let x = pos.x as usize;
                let y = pos.y as usize;

                // Use the map_coordinates function to get the display and local pixel coordinates
                if let Some((display, local_x, local_y)) = self.map_coordinates(x, y) {
                    match color {
                        BinaryColor::On => self.buffer[display][local_y] |= 1 << (7 - local_x),
                        BinaryColor::Off => self.buffer[display][local_y] &= !(1 << (7 - local_x)),
                    }
                }
            });

        Ok(())
    }

    fn fill_contiguous<I>(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        colors: I,
    ) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.draw_iter(
            area.points()
                .zip(colors)
                .map(|(pos, color)| embedded_graphics::Pixel(pos, color)),
        )
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        self.fill_contiguous(area, core::iter::repeat(color))
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.fill_solid(&self.bounding_box(), color)
    }
}


impl<SPI, const ROWS: usize, const COLS: usize> OriginDimensions for Max7219<SPI, ROWS, COLS>
where
    SPI: Write<u8>,
    [u8; ROWS*COLS * 2]: Sized,
{
    fn size(&self) -> Size {
        // The width is `cols * 8` and the height is `rows * 8`
        Size::new((COLS * 8) as u32, (ROWS * 8) as u32)
    }
}
