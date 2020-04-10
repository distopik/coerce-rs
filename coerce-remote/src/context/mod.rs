use crate::actor::message::{
    ClientWrite, GetNodes, PopRequest, PushRequest, RegisterClient, RegisterNode, RegisterNodes,
};
use crate::actor::{
    RemoteClientRegistry, RemoteHandler, RemoteHandlerTypes, RemoteRegistry, RemoteRequest,
    RemoteResponse,
};
use crate::cluster::builder::client::ClusterClientBuilder;
use crate::cluster::builder::worker::ClusterWorkerBuilder;
use crate::cluster::node::RemoteNode;
use crate::codec::RemoteHandlerMessage;
use crate::context::builder::RemoteActorContextBuilder;
use crate::handler::RemoteActorMessageMarker;
use crate::net::client::RemoteClientStream;
use crate::net::message::{CreateActor, SessionEvent};
use crate::storage::activator::ActorActivator;
use coerce_rt::actor::context::ActorContext;
use coerce_rt::actor::message::Message;
use coerce_rt::actor::{Actor, ActorId, ActorRef};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

pub mod builder;

#[derive(Clone)]
pub struct RemoteActorContext {
    node_id: Uuid,
    inner: ActorContext,
    activator: ActorActivator,
    handler_ref: ActorRef<RemoteHandler>,
    registry_ref: ActorRef<RemoteRegistry>,
    clients_ref: ActorRef<RemoteClientRegistry>,
    types: Arc<RemoteHandlerTypes>,
}

impl RemoteActorContext {
    pub fn builder() -> RemoteActorContextBuilder {
        RemoteActorContextBuilder::new()
    }

    pub fn cluster_worker(self) -> ClusterWorkerBuilder {
        ClusterWorkerBuilder::new(self)
    }

    pub fn cluster_client(self) -> ClusterClientBuilder {
        ClusterClientBuilder::new(self)
    }
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum RemoteActorError {
    ActorUnavailable,
}

impl RemoteActorContext {
    pub async fn handle_message(
        &mut self,
        identifier: String,
        actor_id: ActorId,
        buffer: &[u8],
    ) -> Result<Vec<u8>, RemoteActorError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handler = self.types.message_handler(&identifier);

        if let Some(handler) = handler {
            handler.handle(actor_id, self.clone(), buffer, tx).await;
        };

        match rx.await {
            Ok(res) => Ok(res),
            Err(_e) => Err(RemoteActorError::ActorUnavailable),
        }
    }

    pub async fn create_actor(&mut self, args: CreateActor) -> Result<Vec<u8>, RemoteActorError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handler = self.types.actor_handler(&args.actor_type);

        if let Some(handler) = handler {
            handler.create(args, self.clone(), tx).await;
        }

        match rx.await {
            Ok(res) => Ok(res),
            Err(_e) => Err(RemoteActorError::ActorUnavailable),
        }
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    pub fn handler_name<A: Actor, M: Message>(&mut self) -> Option<String>
    where
        A: 'static + Send + Sync,
        M: 'static + Send + Sync,
        M::Result: Send + Sync,
    {
        let marker = RemoteActorMessageMarker::<A, M>::new();
        self.types.handler_name(marker)
    }

    pub fn create_message<A: Actor, M: Message>(
        &mut self,
        id: &ActorId,
        message: M,
    ) -> Option<RemoteHandlerMessage<M>>
    where
        A: 'static + Send + Sync,
        M: 'static + Serialize + Send + Sync,
        M::Result: Send + Sync,
    {
        match self.handler_name::<A, M>() {
            Some(handler_type) => Some(RemoteHandlerMessage {
                actor_id: id.clone(),
                handler_type,
                message,
            }),
            None => None,
        }
    }

    pub async fn register_client<T: RemoteClientStream>(&mut self, node_id: Uuid, client: T)
    where
        T: 'static + Sync + Send,
    {
        self.clients_ref
            .send(RegisterClient(node_id, client))
            .await
            .unwrap()
    }

    pub async fn register_nodes(&mut self, nodes: Vec<RemoteNode>) {
        self.registry_ref.send(RegisterNodes(nodes)).await.unwrap()
    }

    pub async fn notify_register_nodes(&mut self, nodes: Vec<RemoteNode>) {
        self.registry_ref
            .notify(RegisterNodes(nodes))
            .await
            .unwrap()
    }

    pub async fn register_node(&mut self, node: RemoteNode) {
        self.registry_ref.send(RegisterNode(node)).await.unwrap()
    }

    pub async fn get_nodes(&mut self) -> Vec<RemoteNode> {
        self.registry_ref.send(GetNodes).await.unwrap()
    }

    pub async fn send_message(&mut self, node_id: Uuid, message: SessionEvent) {
        self.clients_ref
            .send(ClientWrite(node_id, message))
            .await
            .unwrap()
    }

    pub async fn push_request(
        &mut self,
        id: Uuid,
        res_tx: tokio::sync::oneshot::Sender<RemoteResponse>,
    ) {
        self.handler_ref
            .send(PushRequest(id, RemoteRequest { res_tx }))
            .await
            .expect("push request send");
    }

    pub async fn pop_request(
        &mut self,
        id: Uuid,
    ) -> Option<tokio::sync::oneshot::Sender<RemoteResponse>> {
        match self.handler_ref.send(PopRequest(id)).await {
            Ok(s) => s.map(|s| s.res_tx),
            Err(_) => None,
        }
    }

    pub fn activator_mut(&mut self) -> &mut ActorActivator {
        &mut self.activator
    }

    pub fn activator(&self) -> &ActorActivator {
        &self.activator
    }

    pub fn inner(&mut self) -> &mut ActorContext {
        &mut self.inner
    }
}

pub(crate) trait RemoteActorContextInternal {}
