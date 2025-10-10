#![no_std]
#![no_main]

use esp_hal::rng::Rng;
// --- Standard ESP-HAL and System Crates ---
use esp_backtrace as _;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{clock::CpuClock, esp_riscv_rt::entry};
use esp_println::println;
use esp_wifi::wifi::{AccessPointConfiguration, Configuration};
use esp_wifi::init;
use smoltcp::iface::SocketSet;
use smoltcp::socket::udp;

// --- Networking Crates ---
use core::fmt::Write;
use heapless::string::String;
use smoltcp::{
    socket::{
        tcp::{Socket as TcpSocket, State},
        udp::Socket as UdpSocket,
    },
    time::Instant,
    wire::{IpAddress, IpCidr},
};

// --- Network Configuration Constants ---
const SSID: &str = "ESP32C6-AP";
const PASSWORD: &str = "supersecure";

// AP Static IP configuration
const AP_IP: [u8; 4] = [192, 168, 4, 1];
const AP_NETMASK: [u8; 4] = [255, 255, 255, 0];
const AP_GW: [u8; 4] = [192, 168, 4, 1];

// DHCP Server IP range and settings
const DHCP_POOL_START: [u8; 4] = [192, 168, 4, 10];
const DHCP_POOL_END: [u8; 4] = [192, 168, 4, 100];
const DHCP_LEASE_TIME: u32 = 60 * 60; // 1 hour
const DHCP_MAX_LEASES: usize = 10;

// HTTP Server configuration
const HTTP_PORT: u16 = 80;

// Buffer/Socket Size constants (must be large enough for smoltcp)
const TCP_RX_BUF_SIZE: usize = 1536;
const TCP_TX_BUF_SIZE: usize = 1536;
const UDP_BUF_SIZE: usize = 512;
const SOCKET_COUNT: usize = 8; // Max number of sockets in the set

macro_rules! make_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[entry]
fn main() -> ! {
    // 1. Initialize ESP32-C6 Hardware & Clocks
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    // let mut rtc = Rtc::new(peripherals.LP_CLKRST);

    let timer_group0 = TimerGroup::new(peripherals.TIMG0);
    let mut wdt0 = timer_group0.wdt;
    let timer_group1 = TimerGroup::new(peripherals.TIMG1);
    let mut wdt1 = timer_group1.wdt;

    // Disable watchdogs for simple development
    // rtc.rwdt.disable();
    wdt0.disable();
    wdt1.disable();

    // let _io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    // 2. Wi-Fi Initialization (using esp-wifi)
    let mut rng = Rng::new(peripherals.RNG);
    let esp_wifi_ctrl = init(timer_group0.timer0, rng.clone()).unwrap();

    let (mut controller, interfaces) =
        esp_wifi::wifi::new(&esp_wifi_ctrl, peripherals.WIFI).unwrap();

    let mut device = interfaces.ap;
    let mut iface = create_interface(&mut device);

    // Configure and start Wi-Fi in Access Point mode
    let ap_config = Configuration::AccessPoint(AccessPointConfiguration {
        ssid: SSID.into(),
        password: PASSWORD.into(),
        channel: 6,
        max_connections: 4,
        ..Default::default()
    });

    let res = controller.set_configuration(&ap_config);
    println!("wifi_set_configuration returned {:?}", res);

    controller.start().unwrap();
    println!("is wifi started: {:?}", controller.is_started());

    println!("{:?}", controller.capabilities());

    // 3. smoltcp Network Stack Setup
    let ip_address = IpAddress::v4(192, 168, 4, 1);
    let ip_cidr = IpCidr::new(ip_address, 24); // /24 subnet mask

    // Create the smoltcp Interface

    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs.push(ip_cidr).unwrap(); // Assign the static AP IP
    });

    // 4. smoltcp Socket Storage
    let mut socket_storage = [None; SOCKET_COUNT];
    let socket_set = make_static!(SocketSet<'static>, SocketSet::new(&mut socket_storage[..]));

    // --- 5. DHCP Server Setup ---
    // Allocate static memory for the UDP socket buffers
    let udp_rx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let udp_tx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let dhcp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);

    // Bind the socket to the DHCP server port (67)
    dhcp_socket.bind(67).unwrap();

    let dhcp_handle = socket_set.add(dhcp_socket);

    // Create the DHCP Server instance
    let dhcp_server = DhcpServer::new(
        AP_IP.into(),
        AP_NETMASK.into(),
        AP_GW.into(),
        &[DHCP_POOL_START.into(), DHCP_POOL_END.into()],
        DHCP_LEASE_TIME,
        DHCP_MAX_LEASES,
    )
    .unwrap();

    println!(
        "DHCP Pool configured: {:?} to {:?}",
        DHCP_POOL_START, DHCP_POOL_END
    );

    // --- 6. HTTP Server Setup ---
    // Allocate static memory for the TCP socket buffers
    let tcp_rx_buffer = [0; TCP_RX_BUF_SIZE];
    let tcp_tx_buffer = [0; TCP_TX_BUF_SIZE];
    let tcp_socket = TcpSocket::new(
        smoltcp::socket::tcp::SocketBuffer::new(&mut tcp_rx_buffer[..]),
        smoltcp::socket::tcp::SocketBuffer::new(&mut tcp_tx_buffer[..]),
    );
    let http_handle = socket_set.add(tcp_socket);

    // Start listening for HTTP connections
    let http_socket = socket_set.get::<TcpSocket>(http_handle);
    if let Err(e) = http_socket.listen(HTTP_PORT) {
        println!("Failed to start HTTP server listener: {:?}", e);
    } else {
        println!("HTTP server listening on port {}", HTTP_PORT);
    }

    // 7. Main Loop
    let mut current_time = Instant::from_millis(0);
    loop {
        // Update time for smoltcp stack using the hardware system timer
        let new_millis = 500;
        current_time = Instant::from_millis(new_millis as i64);

        // Process network traffic (smoltcp poll)
        let _ = iface.poll(current_time, &mut device, socket_set);

        // --- DHCP Server Service ---
        // Get the DHCP socket and poll the DHCP server logic
        let mut socket = socket_set.get_mut::<UdpSocket>(dhcp_handle);

        // dhcp_server.poll_udp_socket will check for and handle incoming DHCP requests (from port 67)
        let _ = dhcp_server.poll_udp_socket(current_time, &mut socket);

        // --- HTTP Server Service (TCP state machine) ---
        let http_socket = socket_set.get::<TcpSocket>(http_handle);

        match http_socket.state() {
            // State 1: A connection is open and established
            State::Established => {
                if http_socket.can_recv() {
                    let mut buffer = [0u8; 512];
                    match http_socket.recv_slice(&mut buffer) {
                        Ok(len) => {
                            let request_str = core::str::from_utf8(&buffer[..len])
                                .unwrap_or("Invalid UTF8 Request");

                            // Check for basic GET request (we only serve "/")
                            if request_str.starts_with("GET / ") {
                                // Simple HTML Response Body
                                let mut response_body: String<256> = String::new();
                                write!(response_body, "
                                    <!DOCTYPE html>
                                    <html>
                                        <head>
                                            <title>ESP32C6 AP Server</title>
                                            <meta name='viewport' content='width=device-width, initial-scale=1.0'>
                                            <style>
                                                body {{ font-family: sans-serif; background-color: #f0f4f8; margin: 0; padding: 20px; }}
                                                .card {{ background-color: #fff; padding: 25px; border-radius: 12px; box-shadow: 0 6px 12px rgba(0,0,0,0.15); max-width: 400px; margin: auto; }}
                                                h1 {{ color: #4a90e2; border-bottom: 2px solid #4a90e2; padding-bottom: 10px; }}
                                                p {{ color: #333; }}
                                            </style>
                                        </head>
                                        <body>
                                            <div class='card'>
                                                <h1>Rust on ESP32-C6</h1>
                                                <p><b>Access Point Status:</b> Online</p>
                                                <p><b>DHCP:</b> Active, serving clients.</p>
                                                <p><b>Uptime (ms):</b> {}</p>
                                            </div>
                                        </body>
                                    </html>
                                ", current_time.total_millis()).unwrap();

                                // HTTP Headers
                                let mut response_headers: String<256> = String::new();
                                write!(response_headers, "HTTP/1.1 200 OK\r\n").unwrap();
                                write!(response_headers, "Content-Type: text/html\r\n").unwrap();
                                write!(
                                    response_headers,
                                    "Content-Length: {}\r\n",
                                    response_body.len()
                                )
                                .unwrap();
                                write!(response_headers, "Connection: close\r\n").unwrap();
                                write!(response_headers, "\r\n").unwrap(); // End of headers

                                // Write headers and body to the socket
                                if http_socket.can_send() {
                                    let _ = http_socket.send_slice(response_headers.as_bytes());
                                    let _ = http_socket.send_slice(response_body.as_bytes());
                                    // Shut down the write side and transition to CLOSE_WAIT
                                    http_socket.close();
                                }
                            } else {
                                // 404 response for unhandled paths
                                let response = b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                                let _ = http_socket.send_slice(response);
                                http_socket.close();
                            }
                        }
                        Err(e) => {
                            println!("TCP receive error: {:?}", e);
                            http_socket.close();
                        }
                    }
                }
            }

            // State 2: Connection is closing or waiting for remote to close
            State::CloseWait
            | State::LastAck
            | State::FinWait1
            | State::FinWait2
            | State::Closing => {
                // Ensure the socket is properly closed to free the resource
                http_socket.close();
            }

            // State 3: Listening, waiting for incoming connection (no action needed here)
            State::Listen => {}

            // Other states are transient or handled by poll (like SYN_SENT, SYN_RCVD)
            _ => {}
        }

        // Allow the Wi-Fi driver to handle internal events like beaconing and association.
        // let _ = esp_wifi::tasks::service_tasks();
    }
}

// some smoltcp boilerplate
fn timestamp() -> smoltcp::time::Instant {
    smoltcp::time::Instant::from_micros(
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros() as i64,
    )
}

pub fn create_interface(device: &mut esp_wifi::wifi::WifiDevice) -> smoltcp::iface::Interface {
    // users could create multiple instances but since they only have one WifiDevice
    // they probably can't do anything bad with that
    smoltcp::iface::Interface::new(
        smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ethernet(
            smoltcp::wire::EthernetAddress::from_bytes(&device.mac_address()),
        )),
        device,
        timestamp(),
    )
}
