use crate::actor::context::ActorContext;
use crate::actor::message::{Handler, Message};
use crate::actor::{Actor, ActorId, BoxedActorRef, CoreActorRef, IntoActorId, LocalActorRef};

use crate::actor::lifecycle::ActorLoop;
use crate::actor::system::ActorSystem;
use crate::remote::actor::message::SetRemote;
use crate::remote::system::RemoteActorSystem;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Instant;
use uuid::Uuid;

pub mod timer;

pub struct ActorScheduler {
    pub(crate) actors: HashMap<ActorId, BoxedActorRef>,
    system_id: Uuid,
    remote: Option<RemoteActorSystem>,
}

impl ActorScheduler {
    pub fn new(system_id: Uuid) -> LocalActorRef<ActorScheduler> {
        start_actor(
            ActorScheduler {
                system_id,
                actors: HashMap::new(),
                remote: None,
            },
            "ActorScheduler-0".into_actor_id(),
            ActorType::Anonymous,
            None,
            None,
            None,
        )
    }
}

#[async_trait]
impl Actor for ActorScheduler {
    async fn started(&mut self, _ctx: &mut ActorContext) {
        tracing::trace!(target: "ActorScheduler", "started on system {}", self.system_id);
    }

    async fn stopped(&mut self, _ctx: &mut ActorContext) {
        debug!(
            "scheduler stopping, total tracked actors={}",
            self.actors.len()
        );

        let start_time = Instant::now();
        let stop_results =
            futures::future::join_all(self.actors.iter().map(|(id, actor)| async move {
                debug!(target: "ActorScheduler", "stopping actor (id={})", &actor.actor_id());
                (id.clone(), actor.stop().await)
            }))
            .await;

        debug!(target: "ActorScheduler", "stopped {} actors in {:?}", stop_results.len(), start_time.elapsed());
        for stop_result in stop_results {
            debug!(target: "ActorScheduler", "stopped actor (id={}, stop_successful={})", stop_result.0, stop_result.1.is_ok());
        }

        debug!("scheduler stopped");
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ActorType {
    Tracked,
    Anonymous,
}

impl ActorType {
    pub fn is_tracked(&self) -> bool {
        match &self {
            &ActorType::Tracked => true,
            _ => false,
        }
    }

    pub fn is_anon(&self) -> bool {
        match &self {
            &ActorType::Anonymous => true,
            _ => false,
        }
    }
}

pub struct SetSystem(pub ActorSystem);

impl Message for SetSystem {
    type Result = ();
}

pub struct RegisterActor<A: Actor>
where
    A: 'static + Sync + Send,
{
    pub id: ActorId,
    pub actor_ref: LocalActorRef<A>,
}

impl<A: Actor> Message for RegisterActor<A>
where
    A: 'static + Sync + Send,
{
    type Result = ();
}

pub struct DeregisterActor(pub ActorId);

impl Message for DeregisterActor {
    type Result = ();
}

pub struct GetActor<A: Actor>
where
    A: 'static + Sync + Send,
{
    id: ActorId,
    _a: PhantomData<A>,
}

impl<A: Actor> Message for GetActor<A>
where
    A: 'static + Sync + Send,
{
    type Result = Option<LocalActorRef<A>>;
}

impl<A: Actor> GetActor<A>
where
    A: 'static + Sync + Send,
{
    pub fn new(id: ActorId) -> GetActor<A> {
        GetActor {
            id,
            _a: PhantomData,
        }
    }
}

#[async_trait]
impl Handler<SetRemote> for ActorScheduler {
    async fn handle(&mut self, message: SetRemote, _ctx: &mut ActorContext) {
        self.remote = Some(message.0);
        trace!(target: "ActorScheduler", "actor scheduler is now configured for remoting");
    }
}

#[async_trait]
impl<A: Actor> Handler<RegisterActor<A>> for ActorScheduler
where
    A: 'static + Sync + Send,
{
    async fn handle(&mut self, message: RegisterActor<A>, _ctx: &mut ActorContext) {
        let _ = self
            .actors
            .insert(message.id.clone(), BoxedActorRef::from(message.actor_ref));

        if let Some(remote) = self.remote.as_mut() {
            debug!(
                "[node={}] registering actor with remote registry, actor_id={}",
                remote.node_id(),
                &message.id
            );

            remote.register_actor(message.id.clone(), None);
        }

        debug!(target: "ActorScheduler", "actor {} registered", message.id);
    }
}

#[async_trait]
impl Handler<DeregisterActor> for ActorScheduler {
    async fn handle(&mut self, msg: DeregisterActor, _ctx: &mut ActorContext) -> () {
        if let Some(_a) = self.actors.remove(&msg.0) {
            debug!(target: "ActorScheduler", "de-registered actor {}", msg.0);
        } else {
            warn!(target: "ActorScheduler", "actor {} not found to de-register", msg.0);
        }
    }
}

#[async_trait]
impl<A: Actor> Handler<GetActor<A>> for ActorScheduler
where
    A: 'static + Sync + Send,
{
    async fn handle(
        &mut self,
        message: GetActor<A>,
        _ctx: &mut ActorContext,
    ) -> Option<LocalActorRef<A>> {
        let actor_ref = self.actors.get(&message.id).and_then(|actor| {
            (&actor.0.as_any())
                .downcast_ref::<LocalActorRef<A>>()
                .map(|s| s.clone())
        });

        if let Some(remote) = &self.remote {
            debug!(target: "ActorScheduler", "[node={}] GetActor(actor_id={}) actor_found={}", remote.node_id(), &message.id, actor_ref.is_some())
        } else {
            debug!(target: "ActorScheduler", "[no-remote-attached] GetActor(actor_id={}) actor_found={}", &message.id, actor_ref.is_some())
        }

        actor_ref
    }
}

pub fn start_actor<A: Actor>(
    actor: A,
    id: ActorId,
    actor_type: ActorType,
    on_start: Option<tokio::sync::oneshot::Sender<()>>,
    system: Option<ActorSystem>,
    parent_ref: Option<BoxedActorRef>,
) -> LocalActorRef<A>
where
    A: 'static + Send + Sync,
{
    let _actor_id_clone = id.clone();
    // let actor_id = actor_id_clone.as_str();
    let _actor_type_name = A::type_name();

    // let node_id = if let Some(system) = &system {
    //     if system.is_remote() {
    //         system.remote().node_id()
    //     } else {
    //         0
    //     }
    // } else {
    //     0
    // };
    //
    // tracing::trace_span!(
    //     "ActorScheduler::start_actor",
    //     actor_id = actor_id,
    //     actor_type_name = actor_type_name,
    //     node_id = node_id,
    // );

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let system_id = system.as_ref().map(|s| *s.system_id());

    let actor_ref = LocalActorRef {
        id,
        sender: tx,
        system_id,
    };

    let cloned_ref = actor_ref.clone();
    tokio::spawn(async move {
        ActorLoop::run(
            actor, actor_type, rx, on_start, cloned_ref, parent_ref, system,
        )
        .await;
    });

    actor_ref
}
