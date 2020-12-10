use crate::{
    buffers::Acker,
    event::{self, Event},
    sinks::util::encoding::{EncodingConfig, EncodingConfigWithDefault, EncodingConfiguration},
    topology::config::{DataType, SinkConfig, SinkContext, SinkDescription},
};
use futures::{lock::Mutex, FutureExt, TryFutureExt};
use futures01::{
    future, stream::FuturesUnordered, Async, AsyncSink, Future, Poll, Sink, StartSend, Stream,
};
use pulsar::{
    proto::CommandSendReceipt, Authentication, Error as PulsarError, Producer, Pulsar,
    TokioExecutor,
};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::{collections::HashSet, sync::Arc};

type MetadataFuture<F, M> = future::Join<F, future::FutureResult<M, <F as Future>::Error>>;

#[derive(Debug, Snafu)]
enum BuildError {
    #[snafu(display("creating pulsar producer failed: {}", source))]
    CreatePulsarSink { source: PulsarError },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PulsarSinkConfig {
    address: String,
    topic: String,
    encoding: EncodingConfigWithDefault<Encoding>,
    auth: Option<AuthConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthConfig {
    name: String,  // "token"
    token: String, // <jwt token>
}

#[derive(Clone, Copy, Debug, Derivative, Deserialize, Serialize, Eq, PartialEq)]
#[derivative(Default)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[derivative(Default)]
    Text,
    Json,
}

struct PulsarSink {
    encoding: EncodingConfig<Encoding>,
    producer: Arc<Mutex<Producer<TokioExecutor>>>,
    in_flight: FuturesUnordered<MetadataFuture<SendFuture, usize>>,
    // ack
    seq_head: usize,
    seq_tail: usize,
    pending_acks: HashSet<usize>,
    acker: Acker,
}

type SendFuture = Box<dyn Future<Item = CommandSendReceipt, Error = PulsarError> + 'static + Send>;

inventory::submit! {
    SinkDescription::new_without_default::<PulsarSinkConfig>("pulsar")
}

#[async_trait::async_trait]
#[typetag::serde(name = "pulsar")]
impl SinkConfig for PulsarSinkConfig {
    fn build(&self, _cx: SinkContext) -> crate::Result<(super::RouterSink, super::Healthcheck)> {
        unimplemented!()
    }

    async fn build_async(
        &self,
        cx: SinkContext,
    ) -> crate::Result<(super::RouterSink, super::Healthcheck)> {
        let producer = self
            .create_pulsar_producer()
            .await
            .context(CreatePulsarSink)?;
        let sink = self.clone().new_sink(producer, cx.acker())?;

        let producer = self
            .create_pulsar_producer()
            .await
            .context(CreatePulsarSink)?;
        let hc = healthcheck(producer);
        Ok((Box::new(sink), Box::new(hc.boxed().compat())))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn sink_type(&self) -> &'static str {
        "pulsar"
    }
}

impl PulsarSinkConfig {
    async fn create_pulsar_producer(&self) -> Result<Producer<TokioExecutor>, PulsarError> {
        let mut builder = Pulsar::builder(&self.address, TokioExecutor);
        if let Some(auth) = &self.auth {
            builder = builder.with_auth(Authentication {
                name: auth.name.clone(),
                data: auth.token.as_bytes().to_vec(),
            });
        }
        let pulsar = builder.build().await?;
        pulsar.producer().with_topic(&self.topic).build().await
    }

    fn new_sink(
        self,
        producer: Producer<TokioExecutor>,
        acker: Acker,
    ) -> crate::Result<PulsarSink> {
        Ok(PulsarSink {
            encoding: self.encoding.into(),
            producer: Arc::new(Mutex::new(producer)),
            in_flight: FuturesUnordered::new(),
            seq_head: 0,
            seq_tail: 0,
            pending_acks: HashSet::new(),
            acker,
        })
    }
}

async fn healthcheck(producer: Producer<TokioExecutor>) -> crate::Result<()> {
    producer.check_connection().await.map_err(Into::into)
}

impl Sink for PulsarSink {
    type SinkItem = Event;
    type SinkError = ();

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        let message = encode_event(item, &self.encoding).map_err(|_| ())?;

        let producer = Arc::clone(&self.producer);
        let fut = async move {
            let mut locked = producer.lock().await;
            match locked.send(message.clone()).await {
                Ok(fut) => fut.await,
                Err(e) => Err(e),
            }
        };

        let seqno = self.seq_head;
        self.seq_head += 1;
        self.in_flight
            .push((Box::new(fut.boxed().compat()) as SendFuture).join(future::ok(seqno)));
        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        loop {
            match self.in_flight.poll() {
                Ok(Async::NotReady) => {
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(None)) => {
                    return Ok(Async::Ready(()));
                }
                Ok(Async::Ready(Some((result, seqno)))) => {
                    trace!(
                        "Pulsar sink produced message {:?} from {} at sequence id {}",
                        result.message_id,
                        result.producer_id,
                        result.sequence_id
                    );
                    self.pending_acks.insert(seqno);
                    let mut num_to_ack = 0;
                    while self.pending_acks.remove(&self.seq_tail) {
                        num_to_ack += 1;
                        self.seq_tail += 1;
                    }
                    self.acker.ack(num_to_ack);
                }
                Err(e) => error!("Pulsar sink generated an error: {}", e),
            }
        }
    }
}

fn encode_event(item: Event, encoding: &EncodingConfig<Encoding>) -> crate::Result<Vec<u8>> {
    let log = item.into_log();

    Ok(match encoding.codec() {
        Encoding::Json => serde_json::to_vec(&log)?,
        Encoding::Text => log
            .get(&event::log_schema().message_key())
            .map(|v| v.as_bytes().to_vec())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn pulsar_event_json() {
        let msg = "hello_world".to_owned();
        let mut evt = Event::from(msg.clone());
        evt.as_mut_log().insert("key", "value");
        let result = encode_event(evt, &EncodingConfig::from(Encoding::Json)).unwrap();
        let map: HashMap<String, String> = serde_json::from_slice(&result[..]).unwrap();
        assert_eq!(msg, map[&event::log_schema().message_key().to_string()]);
    }

    #[test]
    fn pulsar_event_text() {
        let msg = "hello_world".to_owned();
        let evt = Event::from(msg.clone());
        let event = encode_event(evt, &EncodingConfig::from(Encoding::Text)).unwrap();

        assert_eq!(&event[..], msg.as_bytes());
    }
}

#[cfg(feature = "pulsar-integration-tests")]
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::test_util::{random_lines_with_stream, random_string, runtime, trace_init};
    use futures::{compat::Future01CompatExt, StreamExt};
    use pulsar::SubType;

    #[test]
    fn pulsar_happy() {
        trace_init();

        let mut rt = runtime();
        rt.block_on_std(async move {
            let num_events = 1_000;
            let (_input, events) = random_lines_with_stream(100, num_events);

            let topic = format!("test-{}", random_string(10));
            let cnf = PulsarSinkConfig {
                address: "pulsar://127.0.0.1:6650".to_owned(),
                topic: topic.clone(),
                encoding: Encoding::Text.into(),
                auth: None,
            };

            let pulsar = Pulsar::<TokioExecutor>::builder(&cnf.address, TokioExecutor)
                .build()
                .await
                .unwrap();
            let mut consumer = pulsar
                .consumer()
                .with_topic(&topic)
                .with_consumer_name("VectorTestConsumer")
                .with_subscription_type(SubType::Shared)
                .with_subscription("VectorTestSub")
                .build::<String>()
                .await
                .unwrap();

            let (acker, ack_counter) = Acker::new_for_testing();
            let producer = cnf.create_pulsar_producer().await.unwrap();
            let sink = cnf.new_sink(producer, acker).unwrap();
            let _ = sink.send_all(events).compat().await.unwrap();
            assert_eq!(
                ack_counter.load(std::sync::atomic::Ordering::Relaxed),
                num_events
            );

            for _ in 0..num_events {
                let msg = match consumer.next().await.unwrap() {
                    Ok(msg) => msg,
                    Err(err) => panic!("{:?}", err),
                };
                consumer.ack(&msg).await.unwrap();
            }
        });
    }
}
