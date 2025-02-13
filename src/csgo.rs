use std::{time::Duration, path::PathBuf};

use csgo_gsi::{Error, GSIConfigBuilder, GSIServer, Subscription};

pub fn build_server(port: u16, game_path: PathBuf) -> Result<GSIServer, Error> {
    let config = GSIConfigBuilder::new("cs2-bp")
        .subscribe_multiple(Subscription::UNRESTRICTED)
        .throttle(Duration::from_millis(50))
        .buffer(Duration::from_millis(50))
        .heartbeat(Duration::from_millis(1000))
        .build();

    let mut server = GSIServer::new(config, port);

    server.install(game_path)?;

    Ok(server)
}
