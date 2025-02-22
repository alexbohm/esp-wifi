#![no_std]
#![no_main]

#[path = "../../examples-util/util.rs"]
mod examples_util;
use examples_util::hal;

use embedded_io::*;
use embedded_svc::ipv4::Interface;
use embedded_svc::wifi::{AccessPointConfiguration, ClientConfiguration, Configuration, Wifi};

use esp_backtrace as _;
use esp_println::{print, println};
use esp_wifi::initialize;
use esp_wifi::wifi::utils::{create_ap_sta_network_interface, ApStaInterface};
use esp_wifi::wifi_interface::WifiStack;
use esp_wifi::{current_millis, EspWifiInitFor};
use hal::clock::ClockControl;
use hal::Rng;
use hal::{peripherals::Peripherals, prelude::*};

use smoltcp::iface::SocketStorage;
use smoltcp::wire::IpAddress;
use smoltcp::wire::Ipv4Address;

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

#[entry]
fn main() -> ! {
    #[cfg(feature = "log")]
    esp_println::logger::init_logger(log::LevelFilter::Info);

    let peripherals = Peripherals::take();

    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::max(system.clock_control).freeze();

    #[cfg(target_arch = "xtensa")]
    let timer = hal::timer::TimerGroup::new(peripherals.TIMG1, &clocks).timer0;
    #[cfg(target_arch = "riscv32")]
    let timer = hal::systimer::SystemTimer::new(peripherals.SYSTIMER).alarm0;
    let init = initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .unwrap();

    let wifi = peripherals.WIFI;

    let mut ap_socket_set_entries: [SocketStorage; 3] = Default::default();
    let mut sta_socket_set_entries: [SocketStorage; 3] = Default::default();

    let ApStaInterface {
        ap_interface,
        sta_interface,
        ap_device,
        sta_device,
        mut controller,
        ap_socket_set,
        sta_socket_set,
    } = create_ap_sta_network_interface(
        &init,
        wifi,
        &mut ap_socket_set_entries,
        &mut sta_socket_set_entries,
    )
    .unwrap();

    let mut wifi_ap_stack = WifiStack::new(ap_interface, ap_device, ap_socket_set, current_millis);
    let wifi_sta_stack = WifiStack::new(sta_interface, sta_device, sta_socket_set, current_millis);

    let client_config = Configuration::Mixed(
        ClientConfiguration {
            ssid: SSID.try_into().unwrap(),
            password: PASSWORD.try_into().unwrap(),
            ..Default::default()
        },
        AccessPointConfiguration {
            ssid: "esp-wifi".try_into().unwrap(),
            ..Default::default()
        },
    );
    let res = controller.set_configuration(&client_config);
    println!("wifi_set_configuration returned {:?}", res);

    controller.start().unwrap();
    println!("is wifi started: {:?}", controller.is_started());

    println!("{:?}", controller.get_capabilities());

    wifi_ap_stack
        .set_iface_configuration(&embedded_svc::ipv4::Configuration::Client(
            embedded_svc::ipv4::ClientConfiguration::Fixed(embedded_svc::ipv4::ClientSettings {
                ip: embedded_svc::ipv4::Ipv4Addr::from(parse_ip("192.168.2.1")),
                subnet: embedded_svc::ipv4::Subnet {
                    gateway: embedded_svc::ipv4::Ipv4Addr::from(parse_ip("192.168.2.1")),
                    mask: embedded_svc::ipv4::Mask(24),
                },
                dns: None,
                secondary_dns: None,
            }),
        ))
        .unwrap();

    println!("wifi_connect {:?}", controller.connect());

    // wait for STA getting an ip address
    println!("Wait to get an ip address");
    loop {
        wifi_sta_stack.work();

        if wifi_sta_stack.is_iface_up() {
            println!("got ip {:?}", wifi_sta_stack.get_ip_info());
            break;
        }
    }

    println!("Start busy loop on main. Connect to the AP `esp-wifi` and point your browser to http://192.168.2.1:8080/");
    println!("Use a static IP in the range 192.168.2.2 .. 192.168.2.255, use gateway 192.168.2.1");

    let mut rx_buffer = [0u8; 1536];
    let mut tx_buffer = [0u8; 1536];
    let mut ap_socket = wifi_ap_stack.get_socket(&mut rx_buffer, &mut tx_buffer);

    let mut sta_rx_buffer = [0u8; 1536];
    let mut sta_tx_buffer = [0u8; 1536];
    let mut sta_socket = wifi_sta_stack.get_socket(&mut sta_rx_buffer, &mut sta_tx_buffer);

    ap_socket.listen(8080).unwrap();

    loop {
        ap_socket.work();

        if !ap_socket.is_open() {
            ap_socket.listen(8080).unwrap();
        }

        if ap_socket.is_connected() {
            println!("Connected");

            let mut time_out = false;
            let wait_end = current_millis() + 20 * 1000;
            let mut buffer = [0u8; 1024];
            let mut pos = 0;
            loop {
                if let Ok(len) = ap_socket.read(&mut buffer[pos..]) {
                    let to_print =
                        unsafe { core::str::from_utf8_unchecked(&buffer[..(pos + len)]) };

                    if to_print.contains("\r\n\r\n") {
                        print!("{}", to_print);
                        println!();
                        break;
                    }

                    pos += len;
                } else {
                    break;
                }

                if current_millis() > wait_end {
                    println!("Timeout");
                    time_out = true;
                    break;
                }
            }

            if !time_out {
                println!("Making HTTP request");
                sta_socket.work();

                sta_socket
                    .open(IpAddress::Ipv4(Ipv4Address::new(142, 250, 185, 115)), 80)
                    .unwrap();

                sta_socket
                    .write(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n")
                    .unwrap();
                sta_socket.flush().unwrap();

                let wait_end = current_millis() + 20 * 1000;
                loop {
                    let mut buffer = [0u8; 512];
                    if let Ok(len) = sta_socket.read(&mut buffer) {
                        ap_socket.write_all(&buffer[..len]).unwrap();
                        ap_socket.flush().unwrap();
                    } else {
                        break;
                    }

                    if current_millis() > wait_end {
                        println!("Timeout");
                        break;
                    }
                }
                println!();

                sta_socket.disconnect();
            }

            ap_socket.close();

            println!("Done\n");
            println!();
        }

        let wait_end = current_millis() + 5 * 1000;
        while current_millis() < wait_end {
            ap_socket.work();
        }
    }
}

fn parse_ip(ip: &str) -> [u8; 4] {
    let mut result = [0u8; 4];
    for (idx, octet) in ip.split(".").into_iter().enumerate() {
        result[idx] = u8::from_str_radix(octet, 10).unwrap();
    }
    result
}
