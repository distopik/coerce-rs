use crate::util::{
    GetStatusRequest, GetStatusResponse, SetStatusRequest, TestActor, TestActorStatus,
};
use std::time::Duration;

use coerce::actor::message::Message;
use coerce::actor::system::ActorSystem;
use coerce::actor::{
    Actor, ActorCreationErr, ActorFactory, ActorRecipe, ActorRef, IntoActor, LocalActorRef,
};
use coerce::persistent::journal::provider::inmemory::InMemoryStorageProvider;
use coerce::persistent::Persistence;
use coerce::remote::cluster::sharding::coordinator::allocation::{
    AllocateShard, AllocateShardResult,
};
use coerce::remote::cluster::sharding::coordinator::{
    ShardCoordinator, ShardHostState, ShardHostStatus, ShardId,
};

use coerce::remote::cluster::sharding::host::stats::GetStats;
use coerce::remote::cluster::sharding::host::ShardHost;
use coerce::remote::cluster::sharding::Sharding;
use coerce::remote::handler::{ActorHandler, RemoteActorHandler};
use coerce::remote::heartbeat::HeartbeatConfig;
use coerce::remote::net::server::RemoteServer;
use coerce::remote::system::{NodeId, RemoteActorSystem};

mod sharding;
pub mod util;

#[macro_use]
extern crate serde;

#[macro_use]
extern crate async_trait;

#[macro_use]
extern crate coerce_macros;

pub struct TestActorRecipe;

impl ActorRecipe for TestActorRecipe {
    fn read_from_bytes(_bytes: &Vec<u8>) -> Option<Self> {
        Some(Self)
    }

    fn write_to_bytes(&self) -> Option<Vec<u8>> {
        Some(vec![])
    }
}

#[derive(Clone)]
pub struct TestActorFactory;

#[async_trait]
impl ActorFactory for TestActorFactory {
    type Actor = TestActor;
    type Recipe = TestActorRecipe;

    async fn create(&self, _recipe: TestActorRecipe) -> Result<TestActor, ActorCreationErr> {
        Ok(TestActor {
            status: None,
            counter: 0,
        })
    }
}

async fn create_shard_coordinator<T: Actor>(
    remote: &RemoteActorSystem,
    node_id: NodeId,
    node_tag: String,
    shard_host: ActorRef<ShardHost>,
) -> LocalActorRef<ShardCoordinator> {
    let mut shard_coordinator = ShardCoordinator::new(
        T::type_name().to_string(),
        shard_host.clone().unwrap_local(),
    );

    shard_coordinator.add_host(ShardHostState {
        node_id,
        node_tag,
        shards: Default::default(),
        actor: shard_host,
        status: ShardHostStatus::Ready,
    });

    let shard_coordinator = shard_coordinator
        .into_actor(Some("ShardCoordinator".to_string()), remote.actor_system())
        .await
        .expect("ShardCoordinator start");

    shard_coordinator
}

async fn create_system(
    persistence: Persistence,
    listen_addr: &str,
    node_id: NodeId,
    seed_addr: Option<&str>,
) -> (RemoteActorSystem, RemoteServer) {
    let sys = ActorSystem::new().to_persistent(persistence);
    let remote = RemoteActorSystem::builder()
        .with_actor_system(sys)
        .with_tag(format!("node-{}", node_id))
        .with_actors(|a| {
            a.with_actor(TestActorFactory)
                .with_handler::<TestActor, GetStatusRequest>("GetStatusRequest")
                .with_handler::<TestActor, SetStatusRequest>("SetStatusRequest")
        })
        .configure(|c| {
            c.heartbeat(HeartbeatConfig {
                interval: Duration::from_millis(500),
                ping_timeout: Duration::from_millis(10),
                unhealthy_node_heartbeat_timeout: Duration::from_millis(750),
                terminated_node_heartbeat_timeout: Duration::from_millis(1000),
            })
        })
        .with_id(node_id)
        .build()
        .await;

    let mut server = remote.clone().cluster_worker().listen_addr(listen_addr);

    if let Some(seed_addr) = seed_addr {
        server = server.with_seed_addr(seed_addr);
    }

    let server = server.start().await;

    (remote, server)
}

#[tokio::test]
pub async fn test_shard_rebalancing_upon_node_termination() {
    util::create_trace_logger();

    let persistence = Persistence::from(InMemoryStorageProvider::new());
    let (remote_a, server_a) = create_system(persistence.clone(), "127.0.0.1:31101", 1, None).await;

    let (remote_b, server_b) = create_system(
        persistence.clone(),
        "127.0.0.1:32101",
        2,
        Some("127.0.0.1:31101"),
    )
    .await;

    let sharding_a = Sharding::<TestActorFactory>::builder(remote_a.clone())
        .build()
        .await;

    let sharding_b = Sharding::<TestActorFactory>::builder(remote_b.clone())
        .build()
        .await;

    let sharded_actor = sharding_a.get("leon".to_string(), Some(TestActorRecipe));

    let _ = sharded_actor
        .send(SetStatusRequest {
            status: TestActorStatus::Active,
        })
        .await;

    let res = sharded_actor
        .send(GetStatusRequest)
        .await
        .expect("get status");

    let expected_status = TestActorStatus::Active;
    assert_eq!(res, GetStatusResponse::Ok(expected_status));

    // stop the system, and start a new one (sharing the same persistence backplane)

    {
        let mut server_a = server_a;
        server_a.stop();
        remote_a.actor_system().shutdown().await;
    }

    // tokio::time::sleep(Duration::from_secs(10)).await;

    // create a reference to the sharded actor without specifying a recipe, which stops the sharding internals from creating the actor if it isn't already running
    let sharded_actor = sharding_b.get("leon".to_string(), None);
    let res_after_losing_node_1 = sharded_actor
        .send(SetStatusRequest {
            status: TestActorStatus::Active,
        })
        .await;

    assert_eq!(res_after_losing_node_1.is_ok(), true);
}
