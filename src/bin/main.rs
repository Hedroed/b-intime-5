#![no_std]
#![no_main]

use core::net::{IpAddr, SocketAddr};

use embassy_executor::Spawner;
use embassy_net::{
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
    Stack,
};
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{clock::CpuClock, config::{self, WatchdogConfig}, rtc_cntl::{Rtc, RwdtStage}, timer::timg::TimerGroup};
use esp_println::println;
use log::{error, info};
use sntpc::{get_time, NtpContext, NtpTimestampGenerator};

/*
// TODO: maybe i should make another crate for this make_static?
/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}
*/

const TIMEZONE: jiff::tz::TimeZone = jiff::tz::get!("UTC");
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

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(size: 150 * 1024);

    let config = esp_hal::Config::default()
        .with_cpu_clock(CpuClock::max());
        // .with_watchdog(WatchdogConfig::default());

    let peripherals = esp_hal::init(config);

    let rtc = Rtc::new(peripherals.LPWR);
    // rtc.rwdt.set_timeout(RwdtStage::Stage0, esp_hal::time::Duration::from_millis(2000));
    // rtc.rwdt.enable();
    // log::info!("RWDT watchdog enabled!");

    esp_println::logger::init_logger_from_env();
    log::set_max_level(log::LevelFilter::Info);

    let timg1 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timg1.timer0);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);
    let nvs = esp_hal_wifimanager::Nvs::new(0x9000, 0x6000).unwrap();

    let wm_settings = esp_hal_wifimanager::WmSettings {
        ssid: "B-intime-5".into(),
        wifi_conn_timeout: 30000,
        esp_reset_timeout: Some(300000), // 5min
        ..Default::default()
    };

    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let wifi_res = esp_hal_wifimanager::init_wm(
        wm_settings,
        &spawner,
        Some(&nvs),
        rng.clone(),
        timg0.timer0,
        peripherals.WIFI,
        None,
    )
    .await
    .expect("wm init");

    log::info!("wifi_res: {wifi_res:?}");

    main_loop(wifi_res.sta_stack, rtc).await

    // loop {
    //     // rtc.rwdt.feed();
    //     log::info!("bump {}", esp_hal::time::Instant::now());
    //     Timer::after_millis(15000).await;
    // }
}

async fn main_loop(stack: Stack<'_>, rtc: Rtc<'_>) {
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
    let now = jiff::Timestamp::from_microsecond(rtc.current_time_us() as i64).unwrap();
    info!("Rtc: {now}");

    loop {
        let addr: IpAddr = ntp_addrs[0].into();
        let result = get_time(
            SocketAddr::from((addr, 123)),
            &socket,
            NtpContext::new(Timestamp {
                rtc: &rtc,
                current_time_us: 0,
            }),
        )
        .await;

        match result {
            Ok(time) => {
                let old_time = rtc.current_time_us() as i64;

                // Set time immediately after receiving to reduce time offset.
                rtc.set_current_time_us(
                    (time.sec() as u64 * USEC_IN_SEC)
                        + ((time.sec_fraction() as u64 * USEC_IN_SEC) >> 32),
                );

                info!(
                    "Response: {:?}\nnew: {}\nold : {}",
                    time,
                    // Create a Jiff Timestamp from seconds and nanoseconds
                    jiff::Timestamp::from_second(time.sec() as i64)
                        .unwrap()
                        .checked_add(
                            jiff::Span::new()
                                .nanoseconds((time.seconds_fraction as i64 * 1_000_000_000) >> 32),
                        )
                        .unwrap()
                        .to_zoned(TIMEZONE),
                    jiff::Timestamp::from_microsecond(old_time)
                        .unwrap()
                        .to_zoned(TIMEZONE)
                );
            }
            Err(e) => {
                error!("Error getting time: {e:?}");
            }
        }

        Timer::after(Duration::from_secs(60)).await;
    }
}
