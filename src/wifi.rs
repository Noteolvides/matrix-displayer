use anyhow::{bail, Result};
use chrono::{DateTime, SecondsFormat};
use chrono_tz::Tz;
use core::str;
use std::ptr::{self, null_mut};
use embedded_svc::http::client::Client;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, hal::peripheral, http::{client::{Configuration, EspHttpConnection}, Method}, io::Write, sntp::{self, SyncStatus}, sys::{settimeofday, timeval}, wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration as WifiConfiguration, EspWifi}
};
use serde_json::{json, Value};

use log::{error, info};
pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,

    sysloop: EspSystemEventLoop,
) -> Result<Box<EspWifi<'static>>> {
    let mut auth_method = AuthMethod::WPA2Personal;
    if ssid.is_empty() {
        bail!("Missing WiFi name")
    }
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        info!("Wifi password is empty");
    }
    // NVS lets the driver store RF calibration data between boots
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;

    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&WifiConfiguration::Client(ClientConfiguration::default()))?;

    info!("Starting wifi...");

    wifi.start()?;

    // ESP32-C3 (SuperMini-style) boards have a weak RF design: at max TX power (~20 dBm)
    // the signal distorts and connecting fails intermittently. Limit to 8.5 dBm (units of 0.25 dBm).
    unsafe {
        esp_idf_svc::sys::esp!(esp_idf_svc::sys::esp_wifi_set_max_tx_power(34))?;
    }

    info!("Scanning...");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            ssid, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
        None
    };

    wifi.set_configuration(&WifiConfiguration::Client(ClientConfiguration {
        ssid: ssid
            .try_into()
            .expect("Could not parse the given SSID into WiFi config"),
        password: pass
            .try_into()
            .expect("Could not parse the given password into WiFi config"),
        channel,
        auth_method,
        ..Default::default()
    }))?;

    info!("Connecting wifi...");

    // Retry: the first attempt sometimes fails on this board/router combination
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        match wifi.connect() {
            Ok(()) => break,
            Err(e) if attempt < MAX_ATTEMPTS => {
                error!("Wifi connect attempt {}/{} failed: {:?}, retrying...", attempt, MAX_ATTEMPTS, e);
                let _ = wifi.disconnect();
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            Err(e) => return Err(e.into()),
        }
    }

    info!("Waiting for DHCP lease...");

    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    unsafe{

        let sntp = sntp::EspSntp::new_default()?;
        info!("SNTP initialized, waiting for status!");
    
        // Sleep while waiting: a busy loop starves the IDLE task and trips the task watchdog
        while sntp.get_sync_status() != SyncStatus::Completed {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    
        info!("SNTP status received!");
        let timer: *mut i64 = ptr::null_mut();
                    
        let timestamp = esp_idf_svc::sys::time(timer);

        let new_time = timeval {
            tv_sec: timestamp,      // Set seconds
            tv_usec: 0,                  // Set microseconds
        };
    
        // Set the system time
        if settimeofday(&new_time, null_mut()) != 0 {
            error!("Failed to set system time");
        } else {
            info!("System time set successfully");
        }
    }

    Ok(Box::new(esp_wifi))
}

#[derive(Clone,Copy)]
pub enum Location {
    Killester,
    CastleGrove,
    CollinsAvenue,
}

impl Location {
    fn stop_params(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Location::Killester => ("8220IR3881", "Killester", "TRAIN_STATION"),
            Location::CastleGrove => ("8220DB000609", "Castle Grove, Clontarf", "BUS_STOP"),
            Location::CollinsAvenue => ("8220DB000529", "Collins Avenue, Killester", "BUS_STOP"),
        }
    }
}

const URL: &str = "https://api-lts.transportforireland.ie/lts/lts/v1/public/departures";


pub fn post_with_time(
    api_key: &str,
    departure_time: DateTime<Tz>,
    location: Location,
) -> Result<[Option<(String, DateTime<Tz>)>; 3]> {
    // Get location-specific parameters
    let (stop_id, stop_name, stop_type) = location.stop_params();

    // Create a new EspHttpClient
    let connection = EspHttpConnection::new(&Configuration {
        use_global_ca_store: true,
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        // Default TX buffer (512 B) is too small for all the request headers -> truncated request -> 400
        buffer_size_tx: Some(2048),
        ..Default::default()
    })?;
    
    let mut client = Client::wrap(connection);

    let binding = departure_time.to_rfc3339_opts(SecondsFormat::Millis, true);
    let departure_time = binding.as_str();
    // Define the request body as a JSON object
    let request_body = json!({
        "departureDate": departure_time,
        "departureTime": departure_time,
        "stopIds": [stop_id],
        "stopName": stop_name,
        "stopType": stop_type,
        "requestTime": departure_time,
        "departureOrArrival": "DEPARTURE",
        "clientTimeZoneOffsetInMS": 0,
        "refresh": false,
    });

    let request_body_str = request_body.to_string();
    // Without content-length the ESP HTTP client sends the body chunked,
    // and the TFI API answers chunked requests with a 500.
    let content_length = request_body_str.len().to_string();

    // Set the headers
    let headers = [
        ("accept", "application/json, text/plain, */*"),
        ("accept-language", "en-US,en;q=0.9"),
        ("content-type", "application/json"),
        ("content-length", content_length.as_str()),
        ("dnt", "1"),
        ("ocp-apim-subscription-key", api_key),
        ("origin", "https://journeyplanner-production.transportforireland.ie"),
        ("priority", "u=1, i"),
        ("sec-ch-ua", "\"Chromium\";v=\"129\", \"Not=A?Brand\";v=\"8\""),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"Windows\""),
        ("sec-fetch-dest", "empty"),
        ("sec-fetch-mode", "cors"),
        ("sec-fetch-site", "same-site"),
        ("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"),
    ];

    // Open a POST request to `url`
    let mut request = client.request(Method::Post, URL, &headers)?;

    // Write the request body to the request
    request.write_all(request_body_str.as_bytes())?;

    // Submit the request and get the response
    let mut response = request.submit()?;
    let status = response.status();


    match status {
        200..=299 => {
            let mut response_body = String::new();
            let mut occurrences = 0;
            let target_str = format!("\"stopRef\":\"{}\"", stop_id);  // Use dynamic stopRef
            let mut buf = [0; 256];

            // Read data in chunks of 256 bytes
            while response_body.match_indices(&target_str).count() < 3 {
                occurrences = 0; // Reset occurrences in each chunk read
                let bytes_read = response.read(&mut buf)?;
                if bytes_read == 0 {
                    break; // End of response
                }

                // Append the chunk to our growing response body
                response_body.push_str(&String::from_utf8_lossy(&buf[..bytes_read]));

                // Count occurrences of the target string in the accumulated response
                for (index, _) in response_body.match_indices(&target_str) {
                    occurrences += 1;
                    if occurrences == 3 {
                        response_body.truncate(index + target_str.len());
                        break;
                    }
                }
            }

            if occurrences < 3 {
                log::error!("Less than three occurrences of the target string were found");
            }

            // Close JSON after the third occurrence
            let truncated_response = format!("{} }}]}}", response_body);

            let v: Value = serde_json::from_str(&truncated_response)
                .map_err(|e| anyhow::anyhow!("Failed to parse JSON response: {}", e))?;
    
            // Extract stopDepartures
            let departures = v["stopDepartures"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Failed to retrieve stop departures array"))?;

            // Collect up to three scheduled departure times and service numbers
            let mut schedule_times = [None, None, None];
            for (i, departure) in departures.iter().enumerate().take(3) {
                let service_number_key= {
                   match location {
                       Location::Killester => {"destination"},
                       _ => {"serviceNumber"},
                   }
                };
                if let (Some(service_number), Some(scheduled_departure)) = (
                    departure[service_number_key].as_str(),
                    departure["realTimeDeparture"].as_str(),
                ) {
                    departure["destination"].as_str();
                    // Attempt to parse the scheduled departure string to DateTime
                    match DateTime::parse_from_rfc3339(scheduled_departure) {
                        Ok(parsed_time) => {
                            schedule_times[i] = Some((service_number.to_string(), parsed_time.with_timezone(&chrono_tz::Tz::Europe__Dublin)));
                        }
                        Err(e) => log::error!("Failed to parse DateTime: {}", e),
                    }
                }
            }

            // Return the array of Option<(String, DateTime)>
            Ok(schedule_times)
        }
        _ => {
            // Read (up to 1 KB of) the error body: the API usually says what's wrong
            let mut buf = [0u8; 1024];
            let mut len = 0;
            while len < buf.len() {
                match response.read(&mut buf[len..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => len += n,
                }
            }
            log::error!(
                "Unexpected response code: {} body: {}",
                status,
                String::from_utf8_lossy(&buf[..len])
            );
            log::error!("Request body was: {}", request_body_str);
            bail!("Unexpected response code: {}", status);
        }
    }
}
