use alloc::rc::Rc;
use esp_radio::Controller;
use core::ops::DerefMut;
use embassy_executor::Spawner;
use embassy_net::{Config, Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::{
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaState},
};
use structs::{AutoSetupSettings, WmInnerSignals, WmReturn};

pub use nvs::Nvs;
pub use structs::{WmError, WmSettings};
pub use utils::get_efuse_mac;

use crate::wifimanager::nvs::SavedSettings;

mod http;
mod ap;
mod nvs;
mod structs;
mod utils;

#[allow(clippy::too_many_arguments)]
pub async fn init_wm(
    settings: WmSettings,
    spawner: &Spawner,
    flash: esp_hal::peripherals::FLASH<'static>,
    mut rng: Rng,
    wifi: WIFI<'static>,
) -> crate::wifimanager::structs::Result<WmReturn> {
    let generated_ssid = settings.ssid.clone();

    let init = crate::mk_static!(Controller<'static>, esp_radio::init()?);

    let (mut controller, interfaces) = esp_radio::wifi::new(init, wifi, Default::default())?;
    controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;

    let mut storage = SavedSettings::new(flash)?;

    let wifi_setup = storage.load()?;

    esp_println::println!("Read wifi_setup from flash: {wifi_setup:?}");
    controller.set_config(&wifi_setup.to_configuration()?)?;
    controller.start_async().await?;

    let mut wifi_connected =
        utils::try_to_wifi_connect(&mut controller, settings.wifi_conn_timeout).await;

    if !wifi_connected {
        esp_println::println!("Starting wifimanager with ssid: {generated_ssid}");

        let wm_signals = Rc::new(WmInnerSignals::new());

        // let configuration = esp_radio::wifi::ModeConfig::ApSta(
        //     Default::default(),
        //     esp_radio::wifi::AccessPointConfig::default().with_ssid(generated_ssid.clone()),
        // );

        let configuration = esp_radio::wifi::ModeConfig::Client(Default::default());

        controller.set_config(&configuration)?;

        utils::spawn_ap(
            &mut rng,
            spawner,
            wm_signals.clone(),
            interfaces.ap,
        )
        .await?;

        // wm_signals
        //     .wifi_conn_info_sig
        //     .signal(env!("WM_CONN", "missing WM_CONN").as_bytes().to_vec());

        // if !controller_started {
        //     controller.start_async().await?;
        // }

        let wifi_setup = wifi_connection_worker(
            settings.clone(),
            wm_signals,
            storage,
            &mut controller,
            configuration,
        )
        .await?;

        controller.set_config(&wifi_setup.to_configuration()?)?;
        if settings.esp_restart_after_connection {
            esp_println::println!("Wifimanager reset after succesfull first connection...");
            Timer::after_millis(1000).await;
            esp_hal::system::software_reset();
        }
    };

    let sta_config = Config::dhcpv4(Default::default());
    let (sta_stack, runner) = embassy_net::new(
        interfaces.sta,
        sta_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<5>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<5>::new())
        },
        rng.random() as u64,
    );

    let stop_signal = Rc::new(Signal::new());
    spawner.spawn(connection(
        settings.wifi_reconnect_time,
        controller,
        stop_signal.clone(),
    ))?;
    spawner.spawn(sta_task(runner))?;

    Ok(WmReturn {
        wifi_init: init,
        sta_stack,
        ip_address: utils::wifi_wait_for_ip(&sta_stack).await,

        stop_signal,
    })
}

async fn wifi_connection_worker(
    settings: WmSettings,
    wm_signals: Rc<WmInnerSignals>,
    mut storage: SavedSettings,
    controller: &mut WifiController<'static>,
    mut configuration: esp_radio::wifi::ModeConfig,
) -> crate::wifimanager::structs::Result<AutoSetupSettings> {
    let start_time = Instant::now();
    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info = wm_signals.wifi_conn_info_sig.wait().await;

            esp_println::println!("trying to connect to: {:?}", setup_info);
            let esp_radio::wifi::ModeConfig::ApSta(ref mut client_conf, _) = configuration
            else {
                return Err(WmError::Other);
            };

            *client_conf = setup_info.to_client_conf()?;

            controller.set_config(&configuration)?;

            let wifi_connected =
                utils::try_to_wifi_connect(controller, settings.wifi_conn_timeout).await;

            if wifi_connected {
                storage.save(&setup_info)?;

                esp_hal_dhcp_server::dhcp_close();

                Timer::after_millis(1000).await;
                wm_signals.signal_end();
                return Ok(setup_info);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let scan_res = controller.scan_with_config_async(Default::default()).await;
            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok(aps) = scan_res {
                for ap in aps {
                    _ = core::fmt::write(
                        wifis.deref_mut(),
                        format_args!("{}: {}\n", ap.ssid, ap.signal_strength),
                    );
                }
            }

            last_scan = Instant::now();
        }

        if let Some(reset_timeout) = settings.esp_reset_timeout {
            if start_time.elapsed().as_millis() >= reset_timeout {
                esp_println::println!("Wifimanager esp reset timeout reached! Resetting..");
                Timer::after_millis(1000).await;
                esp_hal::system::software_reset();
            }
        }

        Timer::after_millis(100).await;
    }
}

#[embassy_executor::task]
async fn connection(
    wifi_reconnect_time: u64,
    mut controller: WifiController<'static>,
    stop_signal: Rc<Signal<CriticalSectionRawMutex, bool>>,
    //stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) {
    esp_println::println!("WIFI Device capabilities: {:?}", controller.capabilities());

    loop {
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            // wait until we're no longer connected
            let res = embassy_futures::select::select(
                controller.wait_for_event(WifiEvent::StaDisconnected),
                stop_signal.wait(),
            )
            .await;

            match res {
                embassy_futures::select::Either::First(_) => {}
                embassy_futures::select::Either::Second(val) => {
                    if val {
                        _ = controller.disconnect_async().await;
                        _ = controller.stop_async().await;
                        esp_println::println!("WIFI radio stopped!");

                        loop {
                            // wait for `restart_wifi()`
                            let val = stop_signal.wait().await;
                            if !val {
                                break;
                            }
                        }

                        _ = controller.start_async().await;
                        esp_println::println!("WIFI radio restarted!");
                    } else {
                        continue;
                    }
                }
            }

            Timer::after(Duration::from_millis(wifi_reconnect_time)).await
        }

        match controller.connect_async().await {
            Ok(_) => {
                esp_println::println!("Wifi connected!");
            }
            Err(e) => {
                esp_println::println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(wifi_reconnect_time)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn sta_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
