use std::path::Path;

use trailhead_server::LocalConfig;
use trailhead_server::creds::CredStore;

/// A local server config with its database and credentials under `dir`.
pub fn local_config(dir: &Path) -> LocalConfig {
    LocalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        db: dir.join("t.db"),
        creds: CredStore { config_dir: dir.into(), keyring: false },
        user_name: "lane".into(),
        pid_file: None,
        instance: None,
    }
}
