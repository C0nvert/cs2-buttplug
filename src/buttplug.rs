use std::collections::{HashMap, HashSet};

use anyhow::{Context, Error};
use buttplug::{
    client::{
        ButtplugClient,
        ButtplugClientEvent, device::ScalarValueCommand
    },
    core::connector::{ButtplugRemoteClientConnector, ButtplugWebsocketClientTransport},
    core::message::serializer::ButtplugClientJSONSerializer,
    util::async_manager
};
use fehler::throws;
use futures::{future::RemoteHandle, StreamExt};
use tokio::sync::broadcast;

use crate::CloseEvent;

#[derive(Clone)]
pub enum ClientEvent {
    DeviceFound(String, u32),
    DeviceLost(String, u32)
}

#[derive(Clone)]
pub enum GuiEvent {
    DeviceToggle(u32, bool),
    None,
}

#[derive(Copy, Clone)]
pub enum BPCommand {
    Vibrate(f64),
    VibrateIndex(f64, u32),
    Stop
}


async fn run_buttplug_catch(
    client_send: Option<tokio::sync::broadcast::Sender<ClientEvent>>, 
    gui_receive: Option<broadcast::Receiver<GuiEvent>>,
    close_receive: broadcast::Receiver<CloseEvent>, 
    client: ButtplugClient,
    transport: ButtplugWebsocketClientTransport,
    rx: broadcast::Receiver<BPCommand>,
) {
    let err = run_buttplug(client_send, gui_receive, close_receive, client, transport, rx).await;
    if let Err(err) = err {
        error!("Buttplug thread error: {}", err);
    }
}

#[throws]
async fn run_buttplug(
    client_send: Option<tokio::sync::broadcast::Sender<ClientEvent>>,
    gui_receive: Option<broadcast::Receiver<GuiEvent>>,
    close_receive: broadcast::Receiver<CloseEvent>, 
    client: ButtplugClient,
    transport: ButtplugWebsocketClientTransport,
    rx: broadcast::Receiver<BPCommand>,
) {
    info!("Launched buttplug.io thread");

    let recv = tokio_stream::wrappers::BroadcastStream::new(rx);
    let close_recv = tokio_stream::wrappers::BroadcastStream::new(close_receive);

    let connector = ButtplugRemoteClientConnector::<ButtplugWebsocketClientTransport, ButtplugClientJSONSerializer>::new(transport);


    info!("Starting buttplug.io client");

    enum Event {
        Buttplug(ButtplugClientEvent),
        Command(BPCommand),
        GuiCommand(GuiEvent),
        CloseCommand,
    }

    let merge_bp_and_commands = tokio_stream::StreamExt::merge(
        client.event_stream().map(Event::Buttplug),
        recv.map(|ev| {
            Event::Command(match ev {
                Ok(ev) => ev,
                Err(_) => BPCommand::Stop, // stop on error
            })
        }),
    );

    let merge_bp_and_commands_and_close = tokio_stream::StreamExt::merge(
        merge_bp_and_commands,
        close_recv.map(|_| Event::CloseCommand),
    );

    // couldn't find a more elegant way of doing this
    let (_, dummy_recv) = tokio::sync::broadcast::channel(1);
    let gui_receive_stream = tokio_stream::wrappers::BroadcastStream::new(gui_receive.unwrap_or(dummy_recv));
    
    let mut merge_bp_and_commands_and_close_and_gui = tokio_stream::StreamExt::merge(
        merge_bp_and_commands_and_close,
        gui_receive_stream.map(|ev| {
            Event::GuiCommand(match ev {
                Ok(ev) => ev,
                Err(_) => GuiEvent::None, // stop on error
            })
        }),
    );

    client.connect(connector).await?;
    client.start_scanning().await.context("Couldn't start buttplug.io device scan")?;

    let mut enabled_devices = HashSet::new();

    while let Some(event) = merge_bp_and_commands_and_close_and_gui.next().await {
        match event {
            Event::Buttplug(ButtplugClientEvent::DeviceAdded(dev)) => {
                if let Some(ref client_send) = client_send {
                    match client_send.send(ClientEvent::DeviceFound(dev.name().clone(), dev.index())) {
                        Ok(_) => {},
                        Err(e) => error!("Error sending client event: {}", e),
                    }
                }
                info!("Intiface: Device added: {}", dev.name());
                enabled_devices.insert(dev.index());
            }
            Event::Buttplug(ButtplugClientEvent::DeviceRemoved(dev)) => {
                if let Some(ref client_send) = client_send {
                    match client_send.send(ClientEvent::DeviceLost(dev.name().clone(), dev.index())) {
                        Ok(_) => {},
                        Err(e) => error!("Error sending client event: {}", e),
                    }
                }
                info!("Intiface: Device removed: {}", dev.name());
                enabled_devices.remove(&dev.index());
            }
            Event::Buttplug(ButtplugClientEvent::ServerDisconnect) => {
                info!("Intiface: server disconnected, shutting down.");
                break; // we're done
            }
            Event::Buttplug(ButtplugClientEvent::ServerConnect) => {
                info!("Intiface: connected to server.");
            }
            Event::Buttplug(ButtplugClientEvent::Error(e)) => {
                info!("Intiface: error {}", e);
            }
            Event::Buttplug(ButtplugClientEvent::PingTimeout) => {
                info!("Intiface: server not responding to ping.");
            }
            Event::Buttplug(ButtplugClientEvent::ScanningFinished) => {
                info!("Intiface: scanning complete.");
            }
            Event::Command(command) => {
                match command {
                    BPCommand::Vibrate(speed) => {
                        for device in client.devices() {
                            if enabled_devices.contains(&device.index()) {
                                info!("Setting speed {} across device {}", speed, &device.name());
                                info!("Sending vibrate speed {} to device {}", speed, &device.name());
                                device.vibrate(&ScalarValueCommand::ScalarValue(speed.min(1.0))).await.context("Couldn't send Vibrate command")?;
                            }
                        }
                    },
                    BPCommand::Stop => {
                        for device in client.devices() {
                            if enabled_devices.contains(&device.index()) {
                                info!("Stopping device {}", &device.name());
                                device.vibrate(&ScalarValueCommand::ScalarValue(0.0)).await.context("Couldn't send Stop command")?;
                            }
                        }
                    },
                    BPCommand::VibrateIndex(speed, index) => {
                        for device in client.devices() {
                            if enabled_devices.contains(&device.index()) {
                                info!("Setting speed {} on index {} on device {}", speed, index, &device.name());
                                let map = HashMap::from([(index, speed.min(1.0))]);
                                device.vibrate(&ScalarValueCommand::ScalarValueMap(map)).await.context("Couldn't send VibrateIndex command")?;
                            }
                        }
                    }
                }
            },
            Event::CloseCommand => {
                info!("Buttplug thread asked to quit");
                client.disconnect().await.expect("Failed to disconnect from buttplug");
            },
            Event::GuiCommand(GuiEvent::DeviceToggle(index, checked)) => {
                info!("Device {} enabled {}", index, checked);
                match checked {
                    true => {
                        enabled_devices.insert(index);
                    },
                    false => {
                        enabled_devices.remove(&index);
                    }
                }
            }
            Event::GuiCommand(GuiEvent::None) => {
                info!("GUI command");
            }
        }
    }

    // And now we're done!
    info!("Exiting buttplug thread");
}

#[throws]
pub fn spawn_run_thread(close_receive: broadcast::Receiver<CloseEvent>, connect_url: &String, client_send: Option<tokio::sync::broadcast::Sender<ClientEvent>>, gui_receive: Option<broadcast::Receiver<GuiEvent>>) -> (broadcast::Sender<BPCommand>, RemoteHandle<()>) {
    info!("Spawning buttplug thread");
    let client_name = "CS2 integration";
    let (send, recv) = broadcast::channel(5);
    let bpclient = ButtplugClient::new(client_name);

    let transport = if connect_url.starts_with("wss://") {
        ButtplugWebsocketClientTransport::new_secure_connector(&connect_url, false)
    } else {
        ButtplugWebsocketClientTransport::new_insecure_connector(&connect_url)
    };

    let handle = async_manager::spawn_with_handle(run_buttplug_catch(
                client_send,
                gui_receive,
                close_receive,
                bpclient,
                transport,
                recv,
            ))?;

    (send, handle)
}
