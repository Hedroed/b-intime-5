#![no_std]
#![no_main]

extern crate esp_println as _; // Ensure defmt global logger is linked

use b_intime_5::display::{Canvas, Screen};
use b_intime_5::{mk_static, wifimanager};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use reqwless::response::Status;
use reqwless::{client::HttpClient, request::RequestBuilder};
use serde::Deserialize;

use core::{
    net::{IpAddr, SocketAddr},
    str::from_utf8_unchecked,
};

use embassy_executor::Spawner;
use embassy_net::{
    dns::{DnsQueryType, DnsSocket},
    tcp::client::{TcpClient, TcpClientState},
    udp::{PacketMetadata, UdpSocket},
    Stack,
};
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    analog::adc::{Adc, AdcConfig, Attenuation},
    gpio::{Level, Output, OutputConfig},
    peripherals,
    spi::{self, master::Spi},
    time::Rate,
    timer::timg::TimerGroup,
    Blocking,
};
use sntpc::{get_time, NtpContext, NtpTimestampGenerator};

#[derive(Clone, Copy)]
struct TimeOffset {
    ntp_base_us: u64,
    local_base_us: u64,
}

type SharedTime = Mutex<CriticalSectionRawMutex, Option<TimeOffset>>;
type SharedTemperature = Mutex<CriticalSectionRawMutex, Option<f32>>;
type SharedLightlevel = Mutex<CriticalSectionRawMutex, u16>;

const TIMEZONE: jiff::tz::TimeZone = jiff::tz::get!("Europe/Paris");
const NTP_SERVER: &str = "time.google.com";

/// Microseconds in a second
const USEC_IN_SEC: u64 = 1_000_000;

#[derive(Clone, Copy)]
struct Timestamp {
    current_time_us: u64,
}

impl NtpTimestampGenerator for Timestamp {
    fn init(&mut self) {
        self.current_time_us = embassy_time::Instant::now().as_micros();
    }

    fn timestamp_sec(&self) -> u64 {
        self.current_time_us / 1_000_000
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        (self.current_time_us % 1_000_000) as u32
    }
}

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(size: 150 * 1024);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    defmt::info!("Init!");

    let sw_int =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let rng = esp_hal::rng::Rng::new();

    let wm_settings = wifimanager::WmSettings {
        ssid: "B-intime-5".into(),
        wifi_conn_timeout: 30000,
        esp_reset_timeout: Some(300000), // 5min
        ..Default::default()
    };

    let wifi_res = wifimanager::init_wm(
        wm_settings,
        &spawner,
        peripherals.FLASH,
        rng,
        peripherals.WIFI,
    )
    .await
    .expect("wm init");

    defmt::info!("wifi_res: {}", defmt::Debug2Format(&wifi_res));

    let stack = wifi_res.sta_stack;

    let config = OutputConfig::default();
    let cs = Output::new(peripherals.GPIO17, Level::High, config);
    let mosi = Output::new(peripherals.GPIO18, Level::High, config);
    let sclk = Output::new(peripherals.GPIO19, Level::High, config);

    let mut spi = spi::master::Spi::new(
        peripherals.SPI2,
        spi::master::Config::default().with_frequency(Rate::from_khz(100)),
    )
    .expect("Failed to initialize SPI")
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_cs(cs);

    Screen::<8>::init(&mut spi);

    let buf = [0x20_u8; 20];
    let canvas = Canvas::<32, 16>::init();
    let mut view = View {
        buf,
        canvas,
        spi: &mut spi,
    };

    // let mut a = Animation::default();
    // view.wifi_loading(&mut a).await;

    let mutex_time = mk_static!(SharedTime, Mutex::new(None));
    let temperature = mk_static!(SharedTemperature, Mutex::new(None));
    let light = mk_static!(SharedLightlevel, Mutex::new(0));

    spawner
        .spawn(lum_loop(peripherals.GPIO2, peripherals.ADC1, light))
        .expect("lum loop");

    spawner
        .spawn(ha_temperature_loop(stack, temperature))
        .expect("temp loop");

    spawner
        .spawn(ntp_loop(stack, mutex_time))
        .expect("ntp loop");

    loop {
        view.view(mutex_time, temperature, light).await;
        Timer::after(Duration::from_secs(17)).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LigthLevel {
    Bright, // v < 2500
    Low,    // v < 4090
    Dark,   // v >= 4090
}

impl From<u16> for LigthLevel {
    fn from(value: u16) -> Self {
        match value {
            n if n < 3000 => LigthLevel::Bright,
            n if n < 4090 => LigthLevel::Low,
            _ => LigthLevel::Dark,
        }
    }
}

#[embassy_executor::task]
async fn lum_loop(
    analog_pin: peripherals::GPIO2<'static>,
    adc1: peripherals::ADC1<'static>,
    light: &'static SharedLightlevel,
) {
    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin(analog_pin, Attenuation::_11dB);
    let mut adc1 = Adc::new(adc1, adc1_config).into_async();

    let mut previous = 0u16;

    loop {
        let pin_value = adc1.read_oneshot(&mut pin).await;

        if previous != pin_value {
            // defmt::info!("new lum {:?}", pin_value);

            let mut l = light.lock().await;
            *l = pin_value;
        }

        Timer::after(Duration::from_secs(7)).await;
        previous = pin_value;
    }
}

use core::fmt::{self, Write};

struct BufWriter<'a> {
    buf: &'a mut [u8],
    pub offset: usize,
}

impl<'a> BufWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        buf.fill(0u8);
        BufWriter { buf, offset: 0 }
    }

    fn len(&self) -> usize {
        self.offset
    }

    // Returns the original buffer (consumed)
    fn into_inner(self) -> &'a mut [u8] {
        self.buf
    }
}

impl<'a> fmt::Write for BufWriter<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();

        // Skip over already-copied data
        let remainder = &mut self.buf[self.offset..];
        // Check if there is space remaining (return error instead of panicking)
        if remainder.len() < bytes.len() {
            return Err(core::fmt::Error);
        }
        // Make the two slices the same length
        let remainder = &mut remainder[..bytes.len()];
        // Copy
        remainder.copy_from_slice(bytes);

        // Update offset to avoid overwriting
        self.offset += bytes.len();

        Ok(())
    }
}

struct View<'a> {
    buf: [u8; 20],
    canvas: Canvas<32, 16>,
    spi: &'a mut Spi<'static, Blocking>,
}

#[allow(dead_code)]
#[derive(Default)]
struct Animation<const T: u32> {
    acc: u32,
}

impl<const T: u32> Animation<T> {
    pub fn step(&mut self) -> u32 {
        let ret = self.acc;

        self.acc += 1;
        if self.acc >= T {
            self.acc = 0;
        }

        ret
    }
}

fn write_buffered<'a>(buf: &'a mut [u8], format: fmt::Arguments) -> &'a str {
    let mut writer = BufWriter::new(buf);

    write!(writer, "{}", format).expect("Can't write");

    let len = writer.len();

    unsafe { from_utf8_unchecked(&writer.into_inner()[..len]) }
}

impl<'a> View<'a> {
    async fn view(
        &mut self,
        time_offset: &SharedTime,
        temperature: &SharedTemperature,
        light: &SharedLightlevel,
    ) {
        let time = {
            let offset_lock = time_offset.lock().await;
            if let Some(offset) = *offset_lock {
                let now = embassy_time::Instant::now().as_micros();
                let current_us = offset.ntp_base_us + now.saturating_sub(offset.local_base_us);
                match jiff::Timestamp::from_microsecond(current_us as i64) {
                    Ok(t) => t.to_zoned(TIMEZONE),
                    Err(_) => jiff::Timestamp::from_second(0).unwrap().to_zoned(TIMEZONE),
                }
            } else {
                jiff::Timestamp::from_second(0).unwrap().to_zoned(TIMEZONE)
            }
        };

        let light_level = light.lock().await;
        let temperature = temperature.lock().await;

        self.canvas.clear();

        if *light_level > 1000 {
            self.canvas.set_pixel(31, 14, true);
        }
        if *light_level > 2000 {
            self.canvas.set_pixel(31, 13, true);
        }
        if *light_level > 2500 {
            self.canvas.set_pixel(31, 12, true);
        }
        if *light_level > 3000 {
            self.canvas.set_pixel(31, 11, true);
        }
        if *light_level > 3500 {
            self.canvas.set_pixel(31, 10, true);
        }
        if *light_level > 4000 {
            self.canvas.set_pixel(31, 9, true);
        }

        let dt = time.datetime();
        let text = write_buffered(
            &mut self.buf,
            format_args!("{:02}:{:02}", dt.hour(), dt.minute()),
        );
        let light_range: LigthLevel = (*light_level).into();
        match light_range {
            LigthLevel::Bright => {
                self.canvas.print_8x8(0, 0, text);
            }
            LigthLevel::Low => {
                self.canvas.print_5x7(4, 2, text);
            }
            LigthLevel::Dark => {
                self.canvas.print_4x4(6, 3, text);
            }
        }

        let text = if let Some(temperature) = *temperature {
            write_buffered(&mut self.buf, format_args!("{:.1}&", temperature))
        } else {
            "N/A&"
        };

        match light_range {
            LigthLevel::Bright => {
                self.canvas.print_5x7(2, 9, text);
            }
            LigthLevel::Low => {
                // self.canvas.print_4x4(6, 10, text);
            }
            LigthLevel::Dark => {
                // self.canvas.print_4x4(6, 8, text);
            }
        }

        Screen::<8>::draw(self.spi, &self.canvas);

        defmt::info!("UPDATE");
    }

    #[allow(dead_code)]
    async fn wifi_loading(&mut self, anim: &mut Animation<3>) {
        let setp = anim.step();

        self.canvas.clear();

        let text = write_buffered(&mut self.buf, format_args!("wifi: {}", setp));
        self.canvas.print_5x7(3, 2, text);

        Screen::<8>::draw(self.spi, &self.canvas);

        defmt::info!("wifi_loading {}", setp);
    }
}

#[embassy_executor::task]
async fn ntp_loop(stack: Stack<'static>, time_offset: &'static SharedTime) -> ! {
    let mut port = 50_000;

    loop {
        stack.wait_config_up().await;

        let addr: IpAddr = match stack.dns_query(NTP_SERVER, DnsQueryType::A).await {
            Ok(addrs) if !addrs.is_empty() => {
                defmt::info!("Resolved NTP DNS: {:?}", addrs);
                addrs[0].into()
            }
            Err(err) => {
                defmt::info!("Failed to resolve NTP DNS {:?}. Using fallback IP...", err);
                IpAddr::V4(core::net::Ipv4Addr::new(216, 239, 35, 12))
            }
            _ => {
                defmt::info!("Failed to resolve NTP DNS. Using fallback IP...");
                IpAddr::V4(core::net::Ipv4Addr::new(216, 239, 35, 12))
            }
        };

        // Fresh socket per request — avoids stale responses causing IncorrectOriginTimestamp.
        // NTP packets are 48 bytes, so minimal buffers suffice.
        let mut rx_meta = [PacketMetadata::EMPTY; 2];
        let mut rx_buffer = [0; 128];
        let mut tx_meta = [PacketMetadata::EMPTY; 2];
        let mut tx_buffer = [0; 128];

        let mut socket = UdpSocket::new(
            stack,
            &mut rx_meta,
            &mut rx_buffer,
            &mut tx_meta,
            &mut tx_buffer,
        );

        if let Err(e) = socket.bind(port) {
            defmt::info!("NTP bind error: {}", e);
        }
        port += 1;
        if port > 60_000 {
            port = 50_000;
        }

        let result = embassy_time::with_timeout(
            Duration::from_secs(10),
            get_time(
                SocketAddr::from((addr, 123)),
                &socket,
                NtpContext::new(Timestamp { current_time_us: 0 }),
            ),
        )
        .await;

        match result {
            Ok(Ok(time)) => {
                defmt::info!("NTP time received: sec={}", time.sec());
                {
                    let mut lock = time_offset.lock().await;
                    *lock = Some(TimeOffset {
                        ntp_base_us: (time.sec() as u64 * USEC_IN_SEC)
                            + ((time.sec_fraction() as u64 * USEC_IN_SEC) >> 32),
                        local_base_us: embassy_time::Instant::now().as_micros(),
                    });
                }

                Timer::after(Duration::from_secs(30 * 60)).await;
            }
            Ok(Err(e)) => {
                defmt::info!("Error getting time: {}", e);
                Timer::after(Duration::from_secs(10)).await;
            }
            Err(_) => {
                defmt::info!("Timeout getting time");
                Timer::after(Duration::from_secs(10)).await;
            }
        }
    }
}

#[derive(Deserialize, Clone)]
struct HAResponse<'a> {
    state: &'a str,
    attributes: HAAttributes,
}

#[derive(Deserialize, Clone)]
struct HAAttributes {
    temperature: f32,
    humidity: usize,
    wind_speed: f32,
}

#[embassy_executor::task]
async fn ha_temperature_loop(stack: Stack<'static>, temperature: &'static SharedTemperature) -> ! {
    let dns = DnsSocket::new(stack);
    let tcp_state = TcpClientState::<1, 4096, 4096>::new();
    let tcp = TcpClient::new(stack, &tcp_state);

    let headers = [(
        "Authorization",
        concat!(
            "Bearer ",
            env!("HA_TOKEN", "no home assistant token provided")
        ),
    )];

    let mut client = HttpClient::new(&tcp, &dns);
    let mut buffer = [0u8; 4096];

    defmt::info!("HA init");
    loop {
        stack.wait_config_up().await;

        let http_req_res = client
            .request(
                reqwless::request::Method::GET,
                env!("HA_URI", "no home assistant uri provided"),
            )
            .await;

        let mut http_req = match http_req_res {
            Ok(req) => req.headers(&headers),
            Err(err) => {
                defmt::error!("HA request init error: {}", err);
                Timer::after(Duration::from_secs(10 * 60)).await;
                continue;
            }
        };

        let response =
            match embassy_time::with_timeout(Duration::from_secs(10), http_req.send(&mut buffer))
                .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => {
                    defmt::error!("HA error 1: {}", err);
                    Timer::after(Duration::from_secs(10)).await;
                    continue;
                }
                Err(_) => {
                    defmt::error!("HA request send timeout");
                    Timer::after(Duration::from_secs(10)).await;
                    continue;
                }
            };

        if response.status != Status::Ok {
            defmt::error!("HA bad status code: {}", response.status);
            Timer::after(Duration::from_secs(10)).await;
            continue;
        }

        let res = match embassy_time::with_timeout(
            Duration::from_secs(10),
            response.body().read_to_end(),
        )
        .await
        {
            Ok(Ok(res)) => res,
            Ok(Err(err)) => {
                defmt::error!("HA error 2: {}", err);
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
            Err(_) => {
                defmt::error!("HA response read timeout");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        match serde_json_core::from_slice::<HAResponse<'_>>(res) {
            Ok((data, _remainder)) => {
                defmt::info!("Temp: {}", data.attributes.temperature);

                {
                    let mut t = temperature.lock().await;
                    *t = Some(data.attributes.temperature);
                }
            }
            Err(err) => {
                defmt::error!("HA error 3: {}", err);
                {
                    let mut t = temperature.lock().await;
                    *t = None;
                }
            }
        }

        Timer::after(Duration::from_secs(10 * 60)).await;
    }
}
