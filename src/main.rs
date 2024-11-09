#![feature(generic_const_exprs)]
mod max7219;
mod wifi;
use anyhow::Result as ResultAny;
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Europe::Dublin;
use chrono_tz::Tz;
use embedded_graphics::mono_font::ascii::FONT_5X8;
use embedded_graphics::prelude::{Dimensions, DrawTarget};
use embedded_graphics::text::Baseline;
use embedded_graphics::Drawable;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::BinaryColor,
    prelude::Point,
    text::Text,
};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::units::FromValueType;
use esp_idf_svc::hal::{
    gpio::AnyIOPin,
    prelude::Peripherals,
    spi::{config, SpiDeviceDriver, SpiDriverConfig},
};
use std::thread;
use std::time::Duration;
use wifi::{post_with_time, Location};

#[toml_cfg::toml_config]
pub struct Config {
    #[default("Wokwi-GUEST")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_psk: &'static str,
    #[default("")]
    api_tfi: &'static str,
}

fn format_departure_times(departures: [Option<(String, DateTime<Tz>)>; 3]) -> String {
    // Get the current time in UTC
    let current_time = Utc::now().with_timezone(&chrono_tz::Tz::Europe__Dublin);

    let mut text = String::new();

    // Loop through each departure and calculate the remaining minutes
    for departure in departures.iter() {
        if let Some((service_number, scheduled_time)) = departure {
            // Calculate the remaining time in minutes
            let duration_until_departure = *scheduled_time - current_time;
            let departure = {
                let n = duration_until_departure.num_minutes();
                match n {
                    n if n <= 0 => format!(" 0m"), // If `n` is 0 or less, return "0"
                    1..=9 => format!(" {}m", n), // Add a leading space for single-digit positive numbers
                    _ => format!("{}m", n),      // No space for numbers 10 and above
                }
            };

            text.push_str(&format!(
                "|{} {}",
                if service_number.len() < 2 {
                    format!("{} ", service_number) // Add a trailing space if less than 2 characters
                } else {
                    service_number.chars().take(2).collect() // Take only the first 2 characters if 2 or more
                },
                departure
            ));
        }
    }
    // Return the formatted string
    text
}

fn main() -> ResultAny<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    let app_config = CONFIG;
    let _wifi = wifi::wifi(
        app_config.wifi_ssid,
        app_config.wifi_psk,
        peripherals.modem,
        sysloop,
    )?;

    let sclk = peripherals.pins.gpio3;
    let mosi = peripherals.pins.gpio1;
    let cs = peripherals.pins.gpio2;
    let spi = peripherals.spi2;

    let config = config::Config::new().baudrate((5).MHz().into());
    let device = SpiDeviceDriver::new_single(
        spi,
        sclk,
        mosi,
        None::<AnyIOPin>,
        Some(cs),
        &SpiDriverConfig::new(),
        &config,
    )?;

    let mut display: max7219::Max7219<_, 3, 15> = max7219::Max7219::new(device);

    // make sure to wake the display up
    display.init()?;
    display.power_on()?;

    let character_style = MonoTextStyle::new(&FONT_5X8, BinaryColor::On);

    loop {
        let current_time = Utc::now().with_timezone(&Dublin);
        let dublin_time = current_time + chrono::Duration::minutes(4);

        // Define locations with a flag to indicate if map processing is required
        let locations = [
            ("KI", Location::Killester, Point::new(0, 0)), // true for mapping
            ("CA", Location::CollinsAvenue, Point::new(0, 8)), // false, no mapping
            ("CG", Location::CastleGrove, Point::new(0, 16)), // false, no mapping
        ];

        for (prefix, location, pos) in &locations {
            let departures = post_with_time(app_config.api_tfi, dublin_time, *location)?;

            // Apply the map function only if `use_map` is true (i.e., only for Killester)
            let departures = match location {
                Location::Killester => departures.map(|entry| match entry {
                    Some((ref text, _)) if text == "Dublin Connolly" => entry,
                    Some((ref text, _)) if text == "Greystones" => entry,
                    Some((ref text, _)) if text == "Bray (Daly)" => entry,
                    _ => None,
                }),
                _ => departures,
            };

            let text = format!("{}{}", prefix, format_departure_times(departures));
            Text::with_baseline(
                &text,
                display.bounding_box().top_left + *pos,
                character_style,
                Baseline::Top,
            )
            .draw(&mut display)?;
        }

        // Draw the updated clock
        Text::with_baseline(
            &format!("{:02}:{:02}", current_time.hour() % 12, current_time.minute()),
            display.bounding_box().top_left + Point::new(95, 0),
            character_style,
            Baseline::Top,
        )
        .draw(&mut display)?;

        display.flush()?;

        thread::sleep(Duration::from_secs(20));
        
        display.clear(BinaryColor::Off)?;

    }
}
