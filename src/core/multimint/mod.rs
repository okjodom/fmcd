//! # Multimint
//!
//! `multimint` is a library for managing Fedimint Clients across multiple
//! federations.
//!
//! The main struct is `MultiMint` which holds a map of `ClientHandleArc`s keyed
//! by `FederationId`, and provides methods for managing and interacting with
//! the clients.
//!
//! Multimint uses 1 top level directory for all its data, and creates
//! subdirectories for each client. Each client's directory behaves like a
//! standalone Fedimint client.
//!
//! Example file tree with 2 clients
//! ```text
//! ├── fm_data_dir
//! │   ├── 15db8cb4f1ec8e484d73b889372bec94812580f929e8148b7437d359af422cd3.db
//! │   ├── 412d2a9338ebeee5957382eb06eac07fa5235087b5a7d5d0a6e18c635394e9ed.db
//! │   ├── multimint.db
//! ```
//!
//! When you create a new `MultiMint` instance you pass it a path to the top
//! level directory for all its data. If the directory does not exist it will be
//! created. If the directory already has data from a previous run, it will be
//! loaded.
//!
//! Example:
//!
//! ```rust
//! use crate::core::multimint::MultiMint;
//! use std::path::PathBuf;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!    let work_dir = PathBuf::from("/path/to/fm_data_dir");
//!
//!    // `new` handles creating a new multimint with no clients or will load the existing databases in the work_dir into ClientHandleArcs
//!    let multimint = MultiMint::new(work_dir).await?;
//!
//!    // List the ids of the federations the multimint has clients for
//!    // E.g. if the work_dir has 2 clients, the ids will be [FederationId, FederationId]
//!    // If there are no clients, the ids will be an empty vector
//!    let federation_ids = multimint.ids().await;
//!    println!("Federation IDs: {:?}", federation_ids);
//!
//!    // Create a new client by connecting to a federation with an invite code
//!    let invite_code = "fed1_invite_code";
//!    // The client's keypair is created based off a 64 byte random secret that is either generated or provided by the user
//!    let secret = env::var("FM_SECRET").ok_or(None);
//!     multimint.register_new(invite_code, secret).await?;
//!    
//!    // Get a client by its federation id
//!    let client = multimint.get(&federation_ids[0]).await?;
//!    println!("Client: {:?}", client);
//!    
//!    Ok(())
//! }
//! ```
//!
//! The `MultiMint` struct provides methods for adding, removing, and updating
//! clients, as well as getting information about the clients and their
//! balances.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bip39::Mnemonic;
use fedimint_bip39::Bip39RootSecretStrategy;
use fedimint_client::secret::RootSecretStrategy;
use fedimint_client::{Client, ClientHandleArc};
use fedimint_core::config::{FederationId, FederationIdPrefix};
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_ln_client::LightningClientModule;
use rand::thread_rng;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub mod client;
pub mod db;

#[cfg(test)]
mod tests;

use self::client::LocalClientBuilder;
use self::db::FederationConfig;

/// `MultiMint` is a struct for managing Fedimint Clients across multiple
/// federations.
#[derive(Debug, Clone)]
pub struct MultiMint {
    db: Database,
    pub client_builder: LocalClientBuilder,
    pub clients: Arc<Mutex<BTreeMap<FederationId, ClientHandleArc>>>,
}

impl MultiMint {
    /// Create a new `MultiMint` instance.
    ///
    /// The `work_dir` parameter is the path to the top level directory for all
    /// its data. If the directory does not exist it will be created. If the
    /// directory already has data from a previous run, it will be loaded.
    ///
    /// # Example
    ///
    /// ```rust
    /// use crate::core::multimint::MultiMint;
    /// use std::path::PathBuf;
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///   let work_dir = PathBuf::from("/path/to/fm_data_dir");
    ///
    ///   // `new` handles creating a new multimint with no clients or will load the existing databases in the work_dir into ClientHandleArcs
    ///  let multimint = MultiMint::new(work_dir).await?;
    ///
    ///   // List the ids of the federations the multimint has clients for
    ///  // E.g. if the work_dir has 2 clients, the ids will be [FederationId, FederationId]
    /// // If there are no clients, the ids will be an empty vector
    /// let federation_ids = multimint.ids().await;
    /// println!("Federation IDs: {:?}", federation_ids);
    ///
    ///   // Create a new client by connecting to a federation with an invite code
    ///   let invite_code = "fed1_invite_code";
    ///  // The client's keypair is created based off a 64 byte random secret that is either generated or provided by the user
    ///  let secret = env::var("FM_SECRET").ok_or(None);
    ///     multimint.register_new(invite_code, secret).await?;
    ///    
    ///   // Get a client by its federation id
    ///   let client = multimint.get(&federation_ids[0]).await?;
    ///   println!("Client: {:?}", client);
    ///    
    ///   Ok(())
    /// }
    /// ```
    pub async fn new(work_dir: PathBuf) -> Result<Self> {
        let db = Database::new(
            fedimint_rocksdb::RocksDb::open(work_dir.join("multimint.db")).await?,
            Default::default(),
        );
        let mnemonic = load_or_generate_mnemonic(&db).await?;

        let client_builder = LocalClientBuilder::new(mnemonic);

        let clients = Arc::new(Mutex::new(BTreeMap::new()));

        Self::load_clients(&mut clients.clone(), &db, &client_builder).await?;

        Ok(Self {
            db,
            client_builder,
            clients,
        })
    }

    /// Load the clients from from the top level database in the work directory
    async fn load_clients(
        clients: &mut Arc<Mutex<BTreeMap<FederationId, ClientHandleArc>>>,
        db: &Database,
        client_builder: &LocalClientBuilder,
    ) -> Result<()> {
        let mut clients = clients.lock().await;

        let dbtx = db.begin_transaction().await;
        let configs = client_builder.load_configs(dbtx.into_nc()).await;

        for config in configs {
            let federation_id = config.invite_code.federation_id();

            if let Ok(client) = client_builder.build(db, config.clone()).await {
                clients.insert(federation_id, client);
            } else {
                warn!("Failed to load client for federation: {federation_id}");
            }
        }

        Ok(())
    }

    /// Register a new client by connecting to a federation with an invite code.
    ///
    /// If the client already exists, it will be updated.
    ///
    /// You can provide a manual secret to use for the client's keypair. If you
    /// don't provide a secret, a 64 byte random secret will be generated, which
    /// you can extract from the client if needed.
    pub async fn register_new(&mut self, invite_code: InviteCode) -> Result<FederationId> {
        let federation_id = invite_code.federation_id();
        if self
            .clients
            .lock()
            .await
            .get(&invite_code.federation_id())
            .is_some()
        {
            warn!(
                "Federation already registered: {:?}",
                invite_code.federation_id()
            );
            return Ok(federation_id);
        }

        let client_cfg = FederationConfig { invite_code };

        let client = self
            .client_builder
            .build(&self.db, client_cfg.clone())
            .await?;

        self.clients.lock().await.insert(federation_id, client);

        let dbtx = self.db.begin_transaction().await;
        self.client_builder
            .save_config(client_cfg.clone(), dbtx)
            .await?;

        Ok(federation_id)
    }

    /// Get all the clients in the multimint.
    pub async fn all(&self) -> Vec<ClientHandleArc> {
        self.clients.lock().await.values().cloned().collect()
    }

    /// Get the ids of the federations the multimint has clients for.
    pub async fn ids(&self) -> Vec<FederationId> {
        self.clients.lock().await.keys().cloned().collect()
    }

    /// Get the federation ids as get_federation_ids for consistency with
    /// services
    pub async fn get_federation_ids(&self) -> Vec<FederationId> {
        self.ids().await
    }

    /// Get a client by its federation id.
    pub async fn get(&self, federation_id: &FederationId) -> Option<ClientHandleArc> {
        self.clients.lock().await.get(federation_id).cloned()
    }

    /// Get a client by its federation id prefix. (Useful for checking if a
    /// client exists for given ecash notes)
    pub async fn get_by_prefix(
        &self,
        federation_id_prefix: &FederationIdPrefix,
    ) -> Option<ClientHandleArc> {
        let keys = self
            .clients
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let federation_id = keys
            .into_iter()
            .find(|id| id.to_prefix() == *federation_id_prefix);

        match federation_id {
            Some(federation_id) => self.get(&federation_id).await,
            None => None,
        }
    }

    /// Update the gateway caches for all the lightning modules in the
    /// multimint.
    pub async fn update_gateway_caches(&self) -> Result<()> {
        let clients = self.clients.lock().await;

        for (federation_id, client) in clients.iter() {
            warn!("Updating gateway cache for {:?}", federation_id);
            let lightning_client = client.get_first_module::<LightningClientModule>();
            if let Ok(lightning_client) = lightning_client {
                if let Err(e) = lightning_client.update_gateway_cache().await {
                    warn!(
                        "Failed to update gateway cache for {:?}: {:?}",
                        federation_id, e
                    );
                }
            } else {
                warn!(
                    "Failed to get lightning client module for {:?}",
                    federation_id
                );
            }
        }

        Ok(())
    }
}

async fn load_or_generate_mnemonic(db: &Database) -> Result<Mnemonic> {
    Ok(
        if let Ok(entropy) = Client::load_decodable_client_secret::<Vec<u8>>(db).await {
            Mnemonic::from_entropy(&entropy)?
        } else {
            let mnemonic = if let Ok(words) = std::env::var("MULTIMINT_MNEMONIC_ENV") {
                info!("Using provided mnemonic from environment variable");
                Mnemonic::parse_in_normalized(bip39::Language::English, words.as_str())?
            } else {
                info!("Generating mnemonic and writing entropy to client storage");
                Bip39RootSecretStrategy::<12>::random(&mut thread_rng())
            };

            Client::store_encodable_client_secret(db, mnemonic.to_entropy()).await?;
            mnemonic
        },
    )
}
