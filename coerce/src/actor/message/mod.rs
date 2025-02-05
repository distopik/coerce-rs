use crate::actor::context::ActorContext;
use crate::actor::Actor;
use std::error::Error;

use crate::actor::metrics::ActorMetrics;
use std::fmt::{Debug, Display, Formatter};

use std::marker::PhantomData;
use std::time::Instant;
use tokio::sync::oneshot;

pub trait Message: 'static + Sync + Send + Sized {
    type Result: 'static + Sync + Send;

    fn into_envelope(self, envelope_type: EnvelopeType) -> Result<Envelope<Self>, MessageWrapErr> {
        match envelope_type {
            EnvelopeType::Local => Ok(Envelope::Local(self)),
            EnvelopeType::Remote => self.as_bytes().map(Envelope::Remote),
        }
    }

    fn as_bytes(&self) -> Result<Vec<u8>, MessageWrapErr> {
        Err(MessageWrapErr::NotTransmittable)
    }

    fn from_envelope(envelope: Envelope<Self>) -> Result<Self, MessageUnwrapErr> {
        match envelope {
            Envelope::Local(msg) => Ok(msg),
            Envelope::Remote(bytes) => Self::from_bytes(bytes),
        }
    }

    fn from_bytes(_: Vec<u8>) -> Result<Self, MessageUnwrapErr> {
        Err(MessageUnwrapErr::NotTransmittable)
    }

    fn read_remote_result(_: Vec<u8>) -> Result<Self::Result, MessageUnwrapErr> {
        Err(MessageUnwrapErr::NotTransmittable)
    }

    fn write_remote_result(_res: Self::Result) -> Result<Vec<u8>, MessageWrapErr> {
        Err(MessageWrapErr::NotTransmittable)
    }

    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn type_name() -> &'static str
    where
        Self: Sized,
    {
        std::any::type_name::<Self>()
    }
}

#[async_trait]
pub trait Handler<M: Message>
where
    Self: Actor,
{
    async fn handle(&mut self, message: M, ctx: &mut ActorContext) -> M::Result;
}

pub struct ActorMessage<A: Actor, M: Message>
where
    A: Handler<M>,
{
    msg: Option<M>,
    sender: Option<oneshot::Sender<M::Result>>,
    created_at: Instant,
    _a: PhantomData<A>,
}

#[async_trait]
pub trait ActorMessageHandler<A: Actor>: Sync + Send {
    async fn handle(&mut self, actor: &mut A, ctx: &mut ActorContext);

    fn name(&self) -> &'static str;
}

#[async_trait]
impl<A: Actor, M: Message> ActorMessageHandler<A> for ActorMessage<A, M>
where
    A: Handler<M>,
{
    async fn handle(&mut self, actor: &mut A, ctx: &mut ActorContext) -> () {
        self.handle(actor, ctx).await;
    }

    fn name(&self) -> &'static str {
        std::any::type_name::<M>()
    }
}

pub(crate) type MessageHandler<A> = Box<dyn ActorMessageHandler<A> + Sync + Send>;

impl<A: Actor, M: Message> ActorMessage<A, M>
where
    A: Handler<M>,
{
    pub fn new(msg: M, sender: Option<oneshot::Sender<M::Result>>) -> ActorMessage<A, M> {
        ActorMessage {
            msg: Some(msg),
            sender,
            created_at: Instant::now(),
            _a: PhantomData,
        }
    }

    pub async fn handle(&mut self, actor: &mut A, ctx: &mut ActorContext) {
        let message_waited_for = self.created_at.elapsed();
        let start = Instant::now();

        let msg = self.msg.take();
        let result = actor.handle(msg.unwrap(), ctx).await;
        let message_processing_took = start.elapsed();

        ActorMetrics::incr_messages_processed(
            A::type_name(),
            M::type_name(),
            message_waited_for,
            message_processing_took,
        );

        match self.sender.take() {
            Some(sender) => match sender.send(result) {
                Ok(_) => trace!(target: "ActorMessage", "sent result successfully"),
                Err(_e) => warn!(target: "ActorMessage", "failed to send result"),
            },
            None => {
                trace!(target: "ActorMessage", "no result consumer, message handling complete");
                return;
            }
        }
    }
}

pub enum Envelope<M> {
    Local(M),
    Remote(Vec<u8>),
}

pub enum EnvelopeType {
    Local,
    Remote,
}

#[derive(Debug, Eq, PartialEq)]
pub enum MessageWrapErr {
    Unknown,
    NotTransmittable,
    SerializationErr,
}

impl Display for MessageWrapErr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            MessageWrapErr::NotTransmittable => write!(f, "Message serialisation not supported, messages must override Message::as_remote_envelop and Message::write_remote_result"),
            MessageWrapErr::SerializationErr => write!(f, "Message failed to serialise"),
            MessageWrapErr::Unknown => write!(f, "Message failed to serialise, unknown error"),
        }
    }
}

impl Error for MessageWrapErr {}

#[derive(Debug, Eq, PartialEq)]
pub enum MessageUnwrapErr {
    Unknown,
    NotTransmittable,
    DeserializationErr,
}

impl Display for MessageUnwrapErr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            MessageUnwrapErr::NotTransmittable => write!(f, "Message deserialisation not supported, messages must override Message::as_remote_envelope, Message::from_remote_envelope, Message::read_remote_result, and Message::write_remote_result"),
            MessageUnwrapErr::DeserializationErr => write!(f, "Message failed to deserialise"),
            MessageUnwrapErr::Unknown => write!(f, "Message failed to deserialise, unknown error"),
        }
    }
}

impl Error for MessageUnwrapErr {}

impl<M> Envelope<M> {
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Envelope::Remote(bytes) => bytes,
            _ => panic!("only remote envelopes can yield bytes"),
        }
    }
}

pub struct Exec<F, A, R>
where
    F: (FnMut(&mut A) -> R),
{
    func: F,
    _a: PhantomData<A>,
}

impl<F, A, R> Exec<F, A, R>
where
    F: (FnMut(&mut A) -> R),
{
    pub fn new(f: F) -> Exec<F, A, R> {
        Exec {
            func: f,
            _a: PhantomData,
        }
    }
}

impl<F, A, R> Message for Exec<F, A, R>
where
    for<'r> F: (FnMut(&mut A) -> R) + 'static + Send + Sync,
    A: Actor,
    R: 'static + Send + Sync,
{
    type Result = R;
}

#[async_trait]
impl<F, A, R> Handler<Exec<F, A, R>> for A
where
    A: Actor,
    F: (FnMut(&mut A) -> R) + 'static + Send + Sync,
    R: 'static + Send + Sync,
{
    async fn handle(&mut self, message: Exec<F, A, R>, _ctx: &mut ActorContext) -> R {
        let message = message;
        let mut func = message.func;

        func(self)
    }
}
