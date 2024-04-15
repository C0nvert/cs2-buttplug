use std::collections::HashMap;

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

#[derive(Copy, Clone)]
pub enum BPCommand {
    Vibrate(f64),
    VibrateIndex(f64, u32),
    Stop
}


async fn run_buttplug_catch(
    close_receive: broadcast::Receiver<CloseEvent>, 
    client: ButtplugClient,
    transport: ButtplugWebsocketClientTransport,
    rx: broadcast::Receiver<BPCommand>,
) {
    let err = run_buttplug(close_receive, client, transport, rx).await;
    if let Err(err) = err {
        error!("Buttplug thread error: {}", err);
    }
}

#[throws]
async fn run_buttplug(
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
    let mut merge_bp_and_commands_and_close = tokio_stream::StreamExt::merge(
        merge_bp_and_commands,
        close_recv.map(|_| Event::CloseCommand),
    );

    client.connect(connector).await?;
    client.start_scanning().await.context("Couldn't start buttplug.io device scan")?;

    while let Some(event) = merge_bp_and_commands_and_close.next().await {
        match event {
            Event::Buttplug(ButtplugClientEvent::DeviceAdded(dev)) => {
                info!("Intiface: Device added: {}", dev.name());
            }
            Event::Buttplug(ButtplugClientEvent::DeviceRemoved(dev)) => {
                info!("Intiface: Device removed: {}", dev.name());
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
                            info!("Setting speed {} across device {}", speed, &device.name());
                            info!("Sending vibrate speed {} to device {}", speed, &device.name());
                            device.vibrate(&ScalarValueCommand::ScalarValue(speed.min(1.0))).await.context("Couldn't send Vibrate command")?;
                        }
                    },
                    BPCommand::Stop => {
                        for device in client.devices() {
                            info!("Stopping device {}", &device.name());
                            device.vibrate(&ScalarValueCommand::ScalarValue(0.0)).await.context("Couldn't send Stop command")?;
                        }
                    },
                    BPCommand::VibrateIndex(speed, index) => {
                        for device in client.devices() {
                            info!("Setting speed {} on index {} on device {}", speed, index, &device.name());
                            let map = HashMap::from([(index, speed.min(1.0))]);
                            device.vibrate(&ScalarValueCommand::ScalarValueMap(map)).await.context("Couldn't send VibrateIndex command")?;
                        }
                    }
                }
            },
            Event::CloseCommand => {
                info!("Buttplug thread asked to quit");
                client.disconnect().await.expect("Failed to disconnect from buttplug");
            },
        }
    }

    // And now we're done!
    info!("Exiting buttplug thread");
}

#[throws]
pub fn spawn_run_thread(close_receive: broadcast::Receiver<CloseEvent>, connect_url: &String) -> (broadcast::Sender<BPCommand>, RemoteHandle<()>) {
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
                close_receive,
                bpclient,
                transport,
                recv,
            ))?;

    (send, handle)
}
