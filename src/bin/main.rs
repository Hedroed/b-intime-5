#![no_std]
#![no_main]

use b_intime_5::display::{Canvas, Screen};
use b_intime_5::{mk_static, wifimanager};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
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
    rtc_cntl::Rtc,
    spi::{self, master::Spi},
    time::Rate,
    timer::timg::TimerGroup,
    Blocking,
};
use sntpc::{get_time, NtpContext, NtpTimestampGenerator};

type SharedRtc = Mutex<CriticalSectionRawMutex, Rtc<'static>>;
type SharedTemperature = Mutex<CriticalSectionRawMutex, Option<f32>>;
type SharedLightlevel = Mutex<CriticalSectionRawMutex, u16>;

const TIMEZONE: jiff::tz::TimeZone = jiff::tz::get!("Europe/Paris");
const NTP_SERVER: &str = "pool.ntp.org";

/// Microseconds in a second
const USEC_IN_SEC: u64 = 1_000_000;

#[derive(Clone, Copy)]
struct Timestamp<'a> {
    rtc: &'a Rtc<'a>,
    current_time_us: u64,
}

impl NtpTimestampGenerator for Timestamp<'_> {
    fn init(&mut self) {
        self.current_time_us = self.rtc.current_time_us();
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

    esp_println::println!("Init!");

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

    esp_println::println!("wifi_res: {wifi_res:?}");

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

    let rtc = Rtc::new(peripherals.LPWR);

    Screen::<8>::init(&mut spi);

    let buf = [0x20_u8; 20];
    let canvas = Canvas::<32, 16>::init();
    let mut view = View { buf, canvas, spi: &mut spi };

    // let mut a = Animation::default();
    // view.wifi_loading(&mut a).await;

    let mutex_rtx = mk_static!(SharedRtc, Mutex::new(rtc));
    let temperature = mk_static!(SharedTemperature, Mutex::new(None));
    let light = mk_static!(SharedLightlevel, Mutex::new(0));

    spawner
        .spawn(lum_loop(peripherals.GPIO2, peripherals.ADC1, light))
        .expect("lum loop");

    spawner
        .spawn(ha_temperature_loop(stack, temperature))
        .expect("temp loop");

    spawner
        .spawn(ntp_loop(stack, mutex_rtx))
        .expect("ntp loop");

    loop {
        view.view(mutex_rtx, temperature, light).await;
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
async fn lum_loop(analog_pin: peripherals::GPIO2<'static>, adc1: peripherals::ADC1<'static>, light: &'static SharedLightlevel) {
    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin(analog_pin, Attenuation::_11dB);
    let mut adc1 = Adc::new(adc1, adc1_config).into_async();

    let mut previous = 0u16;

    loop {
        let pin_value = adc1.read_oneshot(&mut pin).await;

        if previous != pin_value {
            // esp_println::println!("new lum {:?}", pin_value);

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
        BufWriter {
            buf,
            offset: 0,
        }
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
    acc: u32
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
    async fn view(&mut self, rtc: &SharedRtc, temperature: &SharedTemperature, light: &SharedLightlevel) {

        let time = {
            let rtc_lock = rtc.lock().await;
            match jiff::Timestamp::from_microsecond(rtc_lock.current_time_us() as i64) {
                Ok(t) => t.to_zoned(TIMEZONE),
                Err(_) => jiff::Timestamp::from_second(0).unwrap().to_zoned(TIMEZONE),
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

        let text = write_buffered(&mut self.buf, format_args!("{}", time.strftime("%H:%M")));
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

        if light_range == LigthLevel::Bright {
            self.canvas.print_5x7(2, 9, text);
        }

        Screen::<8>::draw(self.spi, &self.canvas);

        esp_println::println!("UPDATE");
    }

    #[allow(dead_code)]
    async fn wifi_loading(&mut self, anim: &mut Animation<3>) {
        let setp = anim.step();

        self.canvas.clear();

        let text = write_buffered(&mut self.buf, format_args!("wifi: {}", setp));
        self.canvas.print_5x7(3, 2, text);

        Screen::<8>::draw(self.spi, &self.canvas);

        esp_println::println!("wifi_loading {}", setp);
    }
}

#[embassy_executor::task]
async fn ntp_loop(stack: Stack<'static>, rtc: &'static SharedRtc) -> ! {

    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0; 4096];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    if let Err(e) = socket.bind(123) {
        esp_println::println!("NTP bind error: {e:?}");
    }

    loop {
        let ntp_addrs = match stack.dns_query(NTP_SERVER, DnsQueryType::A).await {
            Ok(addrs) if !addrs.is_empty() => addrs,
            Err(err) => {
                esp_println::println!("Failed to resolve NTP DNS {:?}. Retrying...", err);
                Timer::after(Duration::from_secs(119)).await;
                continue;
            }
            _ => {
                esp_println::println!("Failed to resolve NTP DNS. Retrying...");
                Timer::after(Duration::from_secs(119)).await;
                continue;
            }
        };
        let addr: IpAddr = ntp_addrs[0].into();

        let result = {
            let rtc_lock = rtc.lock().await;
            get_time(
                SocketAddr::from((addr, 123)),
                &socket,
                NtpContext::new(Timestamp {
                    rtc: &rtc_lock,
                    current_time_us: 0,
                }),
            )
            .await
        };

        match result {
            Ok(time) => {
                let rtc_lock = rtc.lock().await;

                // Set time immediately after receiving to reduce time offset.
                rtc_lock.set_current_time_us(
                    (time.sec() as u64 * USEC_IN_SEC)
                        + ((time.sec_fraction() as u64 * USEC_IN_SEC) >> 32),
                );
            }
            Err(e) => {
                esp_println::println!("Error getting time: {e:?}");
            }
        }
        Timer::after(Duration::from_secs(119)).await;
    }
}

#[derive(Deserialize, Clone)]
struct HAResponse<'a> {
    #[allow(dead_code)]
    state: &'a str,
    attributes: HAAttributes,
}

#[derive(Deserialize, Clone)]
struct HAAttributes {
    temperature: f32,
    #[allow(dead_code)]
    humidity: usize,
    #[allow(dead_code)]
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
    
    loop {
        let http_req_res = client
            .request(
                reqwless::request::Method::GET,
                env!("HA_URI", "no home assistant uri provided"),
            )
            .await;

        let mut http_req = match http_req_res {
            Ok(req) => req.headers(&headers),
            Err(err) => {
                esp_println::println!("HA request init error: {:?}", err);
                Timer::after(Duration::from_secs(10 * 60)).await;
                continue;
            }
        };
        
        let response = match http_req.send(&mut buffer).await {
            Ok(response) => {
                response
            },
            Err(err) => {
                esp_println::println!("HA error 1: {:?}", err);
                continue;
            },
        };

        esp_println::println!("Got response");
        let res = match response.body().read_to_end().await {
            Ok(res) => res,
            Err(err) => {
                esp_println::println!("HA error 2: {:?}", err);
                continue;
            },
        };

        match serde_json_core::from_slice::<HAResponse<'_>>(res) {
            Ok((data, _remainder)) => {
                esp_println::println!("Temp: {}", data.attributes.temperature);

                {
                    let mut t = temperature.lock().await;
                    *t = Some(data.attributes.temperature);
                }

            },
            Err(err) => {
                esp_println::println!("HA error 3: {}", err);
                {
                    let mut t = temperature.lock().await;
                    *t = None;
                }
            },
        }


        Timer::after(Duration::from_secs(10 * 60)).await;
    }
}
