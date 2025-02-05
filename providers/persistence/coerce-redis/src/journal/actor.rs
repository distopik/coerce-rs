use coerce::actor::context::ActorContext;
use coerce::actor::message::{Handler, Message};
use coerce::actor::Actor;
use coerce::persistent::journal::storage::JournalEntry;
use coerce::remote::net::StreamData;
use redis::aio::ConnectionLike;
use redis::{RedisError, RedisResult};
use tokio::sync::oneshot::Sender;

pub(crate) struct RedisJournal<C>(pub C);

pub(crate) struct Write {
    pub key: String,
    pub entry: JournalEntry,
    pub result_channel: Sender<anyhow::Result<()>>,
}

impl Message for Write {
    type Result = ();
}

pub(crate) struct ReadSnapshot(pub String, pub Sender<anyhow::Result<Option<JournalEntry>>>);

impl Message for ReadSnapshot {
    type Result = ();
}

pub(crate) struct ReadMessages {
    pub key: String,
    pub from_sequence: i64,
    pub result_channel: Sender<anyhow::Result<Option<Vec<JournalEntry>>>>,
}

impl Message for ReadMessages {
    type Result = ();
}

pub(crate) struct Delete(pub Vec<String>);

impl Message for Delete {
    type Result = anyhow::Result<()>;
}

impl<C: 'static + Send + Sync> Actor for RedisJournal<C> where C: Clone {}

#[async_trait]
impl<C: 'static + ConnectionLike + Send + Sync> Handler<Write> for RedisJournal<C>
where
    C: Clone,
{
    async fn handle(&mut self, message: Write, _ctx: &mut ActorContext) {
        let connection = self.0.clone();
        let _ = tokio::spawn(async move {
            let mut connection = connection;
            if let Err(e) = redis::cmd("ZADD")
                .arg(message.key)
                .arg(message.entry.sequence)
                .arg(message.entry.write_to_bytes().expect("serialized journal"))
                .query_async::<C, ()>(&mut connection)
                .await
            {
                let err = anyhow::Error::new(e);
                let _ = message.result_channel.send(Err(err));
            } else {
                let _ = message.result_channel.send(Ok(()));
            }
        });
    }
}

#[async_trait]
impl<C: 'static + ConnectionLike + Send + Sync> Handler<ReadSnapshot> for RedisJournal<C>
where
    C: Clone,
{
    async fn handle(&mut self, message: ReadSnapshot, _ctx: &mut ActorContext) {
        let connection = self.0.clone();
        let _ = tokio::spawn(async move {
            let mut connection = connection;

            let data = redis::cmd("ZRANGE")
                .arg(message.0)
                .arg("+inf")
                .arg("-inf")
                .arg("BYSCORE")
                .arg("REV")
                .arg(&["LIMIT", "0", "1"])
                .query_async::<C, Option<Vec<Vec<u8>>>>(&mut connection)
                .await;

            match data {
                Ok(data) => {
                    let _ = message.1.send(Ok(data.map_or(None, |b| {
                        b.into_iter().next().map_or(None, |b| read_journal_entry(b))
                    })));
                }
                Err(err) => {
                    let err = anyhow::Error::new(err);
                    let _ = message.1.send(Err(err));
                }
            }
        });
    }
}

#[async_trait]
impl<C: 'static + ConnectionLike + Send + Sync> Handler<ReadMessages> for RedisJournal<C>
where
    C: Clone,
{
    async fn handle(&mut self, message: ReadMessages, _ctx: &mut ActorContext) {
        let connection = self.0.clone();
        let _ = tokio::spawn(async move {
            let mut connection = connection;

            let data = redis::cmd("ZRANGE")
                .arg(message.key)
                .arg(message.from_sequence)
                .arg("+inf")
                .arg("BYSCORE")
                .query_async::<C, Option<Vec<Vec<u8>>>>(&mut connection)
                .await;

            match data {
                Ok(data) => {
                    let _ = message.result_channel.send(Ok(data.map_or(None, |b| {
                        b.into_iter().map(|b| read_journal_entry(b)).collect()
                    })));
                }
                Err(err) => {
                    let err = anyhow::Error::new(err);
                    let _ = message.result_channel.send(Err(err));
                }
            }
        });
    }
}

#[async_trait]
impl<C: 'static + ConnectionLike + Send + Sync> Handler<Delete> for RedisJournal<C>
where
    C: Clone,
{
    async fn handle(&mut self, message: Delete, _ctx: &mut ActorContext) -> anyhow::Result<()> {
        let _ = redis::cmd("DEL")
            .arg(message.0)
            .query_async(&mut self.0)
            .await?;

        Ok(())
    }
}

fn read_journal_entry(redis_value: Vec<u8>) -> Option<JournalEntry> {
    Some(JournalEntry::read_from_bytes(redis_value).unwrap())
}
