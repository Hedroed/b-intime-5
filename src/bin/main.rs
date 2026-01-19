#![no_std]
#![no_main]

use b_intime_5::display::{Canvas, Screen};
use b_intime_5::wifimanager;
use reqwless::{client::HttpClient, request::RequestBuilder};
use serde::Deserialize;

use core::u16;
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
use esp_println::println;
use sntpc::{get_time, NtpContext, NtpResult, NtpTimestampGenerator};

// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
// macro_rules! mk_static {
//     ($t:ty,$val:expr) => {{
//         static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
//         #[deny(unused_attributes)]
//         let x = STATIC_CELL.uninit().write(($val));
//         x
//     }};
// }

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

    let rtc = Rtc::new(peripherals.LPWR);
    // rtc.rwdt.set_timeout(RwdtStage::Stage0, esp_hal::time::Duration::from_millis(2000));
    // rtc.rwdt.enable();
    esp_println::println!("RWDT watchdog enabled!");

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
        rng.clone(),
        peripherals.WIFI,
    )
    .await
    .expect("wm init");

    esp_println::println!("wifi_res: {wifi_res:?}");

    let config = OutputConfig::default();
    let cs = Output::new(peripherals.GPIO17, Level::High, config);
    let mosi = Output::new(peripherals.GPIO18, Level::High, config);
    let sclk = Output::new(peripherals.GPIO19, Level::High, config);

    let mut spi = spi::master::Spi::new(
        peripherals.SPI2,
        spi::master::Config::default().with_frequency(Rate::from_khz(100)),
    )
    .unwrap()
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_cs(cs);

    spawner
        .spawn(lum_loop(peripherals.GPIO2, peripherals.ADC1))
        .expect("lum loop");

    main_loop(wifi_res.sta_stack, rtc, &mut spi).await
}

#[embassy_executor::task]
async fn lum_loop(analog_pin: peripherals::GPIO2<'static>, adc1: peripherals::ADC1<'static>) {
    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin(analog_pin, Attenuation::_11dB);
    let mut adc1 = Adc::new(adc1, adc1_config).into_async();

    let mut previous = u16::MIN;

    loop {
        let pin_value: u16 = adc1.read_oneshot(&mut pin).await;

        if previous != pin_value {
            esp_println::println!("new lum {}", pin_value);
        }

        Timer::after(Duration::from_secs(1)).await;
        previous = pin_value;
    }
}

use core::fmt::{self, Write};

struct Wrapper<'a> {
    buf: &'a mut [u8],
    pub offset: usize,
}

impl<'a> Wrapper<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        buf.fill(0u8);
        Wrapper {
            buf: buf,
            offset: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[0..self.offset]
    }
}

impl<'a> fmt::Write for Wrapper<'a> {
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

enum LigthLevel {
    Bright,  // v < 2500
    Low,  // v < 4090
    Dark,  // V >= 4090
}

struct State {
    rtc: Rtc<'static>,
    temperature: f32,
    light_level: LigthLevel,
}

enum Event {
    Ntp(NtpResult),
    Light(LigthLevel),
    Temperature(f32),
    Reset,
}

async fn main_loop(stack: Stack<'static>, rtc: Rtc<'static>, spi: &mut Spi<'static, Blocking>) {
    let buf = [0x20 as u8; 20];

    let canvas = Canvas::<32, 16>::init();

    Screen::<8>::init(spi);
    let mut view = View { buf, canvas, spi };

    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0; 4096];

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    let ha_res = access_website(stack.clone()).await;

    let state = State {
        rtc,
        temperature: ha_res.temperature,
        light_level: LigthLevel::Bright,
    };

    let ntp_addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.unwrap();

    if ntp_addrs.is_empty() {
        panic!("Failed to resolve DNS. Empty result");
    }

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    socket.bind(123).unwrap();

    // Display initial Rtc time before synchronization
    let now = jiff::Timestamp::from_microsecond(state.rtc.current_time_us() as i64)
        .unwrap()
        .to_zoned(TIMEZONE);
    esp_println::println!("Rtc: {}", now.strftime("%H%M"));

    loop {
        let addr: IpAddr = ntp_addrs[0].into();
        let result = get_time(
            SocketAddr::from((addr, 123)),
            &socket,
            NtpContext::new(Timestamp {
                rtc: &state.rtc,
                current_time_us: 0,
            }),
        )
        .await;

        match result {
            Ok(time) => {
                // Set time immediately after receiving to reduce time offset.
                state.rtc.set_current_time_us(
                    (time.sec() as u64 * USEC_IN_SEC)
                        + ((time.sec_fraction() as u64 * USEC_IN_SEC) >> 32),
                );

                view.view(&state).await;
            }
            Err(e) => {
                esp_println::println!("Error getting time: {e:?}");
            }
        }

        Timer::after(Duration::from_secs(60)).await;
    }
}

struct View<'a> {
    buf: [u8; 20],
    canvas: Canvas<32, 16>,
    spi: &'a mut Spi<'static, Blocking>,
}

impl<'a> View<'a> {
    async fn view(&mut self, state: &State) {
        let time = jiff::Timestamp::from_microsecond(state.rtc.current_time_us() as i64)
            .unwrap()
            .to_zoned(TIMEZONE);

        let mut buf = Wrapper::new(&mut self.buf);
        write!(buf, "{}", time.strftime("%H:%M")).expect("Can't write");
        self.canvas
            .print_8x8(0, 0, unsafe { from_utf8_unchecked(&buf.as_bytes()) });

        let mut buf = Wrapper::new(&mut self.buf);
        write!(buf, "{:.1}&", state.temperature).expect("Can't write");
        self.canvas
            .print_5x7(2, 9, unsafe { from_utf8_unchecked(&buf.as_bytes()) });

        Screen::<8>::draw(&mut self.spi, &self.canvas);

        esp_println::println!("UPDATE");
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

async fn access_website(stack: Stack<'_>) -> HAAttributes {
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];
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
    let mut http_req = client
        .request(
            reqwless::request::Method::GET,
            env!("HA_URI", "no home assistant uri provided"),
        )
        .await
        .unwrap()
        .headers(&headers);
    let response = http_req.send(&mut buffer).await.unwrap();

    esp_println::println!("Got response");
    let res = response.body().read_to_end().await.unwrap();

    let (data, _remainder) = serde_json_core::from_slice::<HAResponse<'_>>(res).unwrap();

    esp_println::println!("Temp: {}", data.attributes.temperature);
    return data.attributes;
}
