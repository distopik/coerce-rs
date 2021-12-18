use crate::actor::pubsub::ChatStreamTopic;
use crate::actor::stream::{ChatMessage, ChatStream, ChatStreamFactory, Join};
use crate::websocket::start;
use chrono::Local;
use coerce::actor::system::ActorSystem;
use coerce::persistent::journal::provider::inmemory::InMemoryStorageProvider;
use coerce::remote::cluster::sharding::coordinator::allocation::AllocateShard;
use coerce::remote::cluster::sharding::coordinator::ShardCoordinator;
use coerce::remote::cluster::sharding::host::request::RemoteEntityRequest;
use coerce::remote::cluster::sharding::shard::Shard;
use coerce::remote::cluster::sharding::Sharding;
use coerce::remote::system::builder::RemoteActorSystemBuilder;
use coerce::remote::system::{NodeId, RemoteActorSystem};
use log::LevelFilter;
use std::io::Write;
use tokio::task::JoinHandle;

pub struct ShardedChatConfig {
    pub node_id: NodeId,
    pub remote_listen_addr: String,
    pub remote_seed_addr: Option<String>,
    pub websocket_listen_addr: String,
}

pub struct ShardedChat {
    system: RemoteActorSystem,
    sharding: Sharding<ChatStreamFactory>,
    listen_task: Option<JoinHandle<()>>,
}

impl ShardedChat {
    pub async fn start(config: ShardedChatConfig) -> ShardedChat {
        let system = create_actor_system(&config).await;
        let sharding = Sharding::start(system.clone()).await;
        let listen_task = tokio::spawn(start(
            config.websocket_listen_addr,
            system.actor_system().clone(),
            sharding.clone(),
        ));

        ShardedChat {
            system,
            sharding,
            listen_task: Some(listen_task),
        }
    }

    pub async fn stop(&mut self) {
        self.system.actor_system().shutdown().await;
        if let Some(listen_task) = self.listen_task.take() {
            listen_task.abort();
        }
    }
}

async fn create_actor_system(config: &ShardedChatConfig) -> RemoteActorSystem {
    let system = ActorSystem::new_persistent(InMemoryStorageProvider::new());
    let remote_system = RemoteActorSystemBuilder::new()
        .with_id(config.node_id)
        .with_actor_system(system)
        .with_distributed_streams(|s| s.add_topic::<ChatStreamTopic>())
        .with_handlers(|handlers| {
            handlers
                .with_actor(ChatStreamFactory)
                .with_handler::<ChatStream, Join>("ChatStream.Join")
                .with_handler::<ChatStream, ChatMessage>("ChatStream.ChatMessage")
        })
        .build()
        .await;

    let mut cluster_worker = remote_system
        .clone()
        .cluster_worker()
        .listen_addr(config.remote_listen_addr.clone());

    if let Some(seed_addr) = &config.remote_seed_addr {
        cluster_worker = cluster_worker.with_seed_addr(seed_addr.clone());
    }

    cluster_worker.start().await;
    remote_system
}
