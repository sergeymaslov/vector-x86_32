use crate::{
    dns::Resolver,
    event::{self, Event},
    region::RegionOrEndpoint,
    serde::to_string,
    sinks::util::{
        encoding::{EncodingConfigWithDefault, EncodingConfiguration},
        retries2::RetryLogic,
        rusoto,
        service2::{ServiceBuilderExt, TowerCompat, TowerRequestConfig},
        sink::Response,
        BatchConfig, BatchSettings, Buffer, Compression, PartitionBatchSink, PartitionBuffer,
        PartitionInnerBuffer,
    },
    template::Template,
    topology::config::{DataType, SinkConfig, SinkContext, SinkDescription},
};
use bytes05::Bytes;
use chrono::Utc;
use futures::{future::BoxFuture, FutureExt, TryFutureExt};
use futures01::{stream::iter_ok, Sink};
use http::StatusCode;
use lazy_static::lazy_static;
use rusoto_core::RusotoError;
use rusoto_s3::{
    HeadBucketRequest, PutObjectError, PutObjectOutput, PutObjectRequest, S3Client, S3,
};
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::collections::BTreeMap;
use std::convert::{TryFrom, TryInto};
use std::task::Context;
use std::task::Poll;
use tower03::{Service, ServiceBuilder};
use tracing::field;
use tracing_futures::Instrument;
use uuid::Uuid;

#[derive(Clone)]
pub struct S3Sink {
    client: S3Client,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct S3SinkConfig {
    pub bucket: String,
    pub key_prefix: Option<String>,
    pub filename_time_format: Option<String>,
    pub filename_append_uuid: Option<bool>,
    pub filename_extension: Option<String>,
    #[serde(flatten)]
    options: S3Options,
    #[serde(flatten)]
    pub region: RegionOrEndpoint,
    #[serde(
        skip_serializing_if = "crate::serde::skip_serializing_if_default",
        default
    )]
    pub encoding: EncodingConfigWithDefault<Encoding>,
    #[serde(default = "Compression::default_gzip")]
    pub compression: Compression,
    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub request: TowerRequestConfig,
    pub assume_role: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct S3Options {
    acl: Option<S3CannedAcl>,
    grant_full_control: Option<String>,
    grant_read: Option<String>,
    grant_read_acp: Option<String>,
    grant_write_acp: Option<String>,
    server_side_encryption: Option<S3ServerSideEncryption>,
    ssekms_key_id: Option<String>,
    storage_class: Option<S3StorageClass>,
    tags: Option<BTreeMap<String, String>>,
    content_encoding: Option<String>, // inherit from compression value
    content_type: Option<String>,     // default `text/x-log`
}

#[derive(Clone, Copy, Debug, Derivative, Deserialize, Serialize)]
#[derivative(Default)]
#[serde(rename_all = "kebab-case")]
enum S3CannedAcl {
    #[derivative(Default)]
    Private,
    PublicRead,
    PublicReadWrite,
    AwsExecRead,
    AuthenticatedRead,
    LogDeliveryWrite,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum S3ServerSideEncryption {
    #[serde(rename = "AES256")]
    AES256,
    #[serde(rename = "aws:kms")]
    AwsKms,
}

#[derive(Clone, Copy, Debug, Derivative, Deserialize, Serialize)]
#[derivative(Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum S3StorageClass {
    #[derivative(Default)]
    Standard,
    ReducedRedundancy,
    IntelligentTiering,
    StandardIA,
    OnezoneIA,
    Glacier,
    DeepArchive,
}

lazy_static! {
    static ref REQUEST_DEFAULTS: TowerRequestConfig = TowerRequestConfig {
        in_flight_limit: Some(50),
        rate_limit_num: Some(250),
        ..Default::default()
    };
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone, Derivative)]
#[serde(rename_all = "snake_case")]
#[derivative(Default)]
pub enum Encoding {
    #[derivative(Default)]
    Text,
    Ndjson,
}

inventory::submit! {
    SinkDescription::new::<S3SinkConfig>("aws_s3")
}

#[typetag::serde(name = "aws_s3")]
impl SinkConfig for S3SinkConfig {
    fn build(&self, cx: SinkContext) -> crate::Result<(super::RouterSink, super::Healthcheck)> {
        let client = self.create_client(cx.resolver())?;
        let healthcheck = self.clone().healthcheck(client.clone()).boxed().compat();
        let sink = self.new(client, cx)?;
        Ok((sink, Box::new(healthcheck)))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn sink_type(&self) -> &'static str {
        "aws_s3"
    }
}

#[derive(Debug, Snafu)]
enum HealthcheckError {
    #[snafu(display("Invalid credentials"))]
    InvalidCredentials,
    #[snafu(display("Unknown bucket: {:?}", bucket))]
    UnknownBucket { bucket: String },
    #[snafu(display("Unknown status code: {}", status))]
    UnknownStatus { status: StatusCode },
}

impl S3SinkConfig {
    pub fn new(&self, client: S3Client, cx: SinkContext) -> crate::Result<super::RouterSink> {
        let request = self.request.unwrap_with(&REQUEST_DEFAULTS);
        let encoding = self.encoding.clone();

        let compression = self.compression;
        let filename_time_format = self
            .filename_time_format
            .clone()
            .unwrap_or_else(|| "%s".into());
        let filename_append_uuid = self.filename_append_uuid.unwrap_or(true);
        let batch = self
            .batch
            .use_size_as_bytes()?
            .get_settings_or_default(BatchSettings::default().bytes(10_000_000).timeout(300));

        let key_prefix = self.key_prefix.as_deref().unwrap_or("date=%F/");
        let key_prefix = Template::try_from(key_prefix)?;

        let s3 = S3Sink { client };

        let filename_extension = self.filename_extension.clone();
        let bucket = self.bucket.clone();
        let options = self.options.clone();

        let svc = ServiceBuilder::new()
            .map(move |req| {
                build_request(
                    req,
                    filename_time_format.clone(),
                    filename_extension.clone(),
                    filename_append_uuid,
                    compression,
                    bucket.clone(),
                    options.clone(),
                )
            })
            .settings(request, S3RetryLogic)
            .service(s3);

        let buffer = PartitionBuffer::new(Buffer::new(batch.size, self.compression));

        let sink =
            PartitionBatchSink::new(TowerCompat::new(svc), buffer, batch.timeout, cx.acker())
                .with_flat_map(move |e| iter_ok(encode_event(e, &key_prefix, &encoding)))
                .sink_map_err(|error| error!("Sink failed to flush: {}", error));

        Ok(Box::new(sink))
    }

    pub async fn healthcheck(self, client: S3Client) -> crate::Result<()> {
        let req = client.head_bucket(HeadBucketRequest {
            bucket: self.bucket.clone(),
        });

        match req.await {
            Ok(_) => Ok(()),
            Err(error) => Err(match error {
                RusotoError::Unknown(resp) => match resp.status {
                    StatusCode::FORBIDDEN => HealthcheckError::InvalidCredentials.into(),
                    StatusCode::NOT_FOUND => HealthcheckError::UnknownBucket {
                        bucket: self.bucket,
                    }
                    .into(),
                    status => HealthcheckError::UnknownStatus { status }.into(),
                },
                error => error.into(),
            }),
        }
    }

    pub fn create_client(&self, resolver: Resolver) -> crate::Result<S3Client> {
        let region = (&self.region).try_into()?;
        let client = rusoto::client(resolver)?;

        let creds = rusoto::AwsCredentialsProvider::new(&region, self.assume_role.clone())?;

        Ok(S3Client::new_with(client, creds, region))
    }
}

impl Service<Request> for S3Sink {
    type Response = PutObjectOutput;
    type Error = RusotoError<PutObjectError>;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let options = request.options;

        let content_encoding = request.content_encoding;
        let content_encoding = options
            .content_encoding
            .or_else(|| content_encoding.map(|ce| ce.to_string()));
        let content_type = options
            .content_type
            .or_else(|| Some("text/x-log".to_owned()));

        let mut tagging = url::form_urlencoded::Serializer::new(String::new());
        if let Some(tags) = options.tags {
            for (p, v) in tags {
                tagging.append_pair(&p, &v);
            }
        }
        let tagging = tagging.finish();

        let client = self.client.clone();
        let request = PutObjectRequest {
            body: Some(request.body.into()),
            bucket: request.bucket,
            key: request.key,
            content_encoding,
            content_type,
            acl: options.acl.map(to_string),
            grant_full_control: options.grant_full_control,
            grant_read: options.grant_read,
            grant_read_acp: options.grant_read_acp,
            grant_write_acp: options.grant_write_acp,
            server_side_encryption: options.server_side_encryption.map(to_string),
            ssekms_key_id: options.ssekms_key_id,
            storage_class: options.storage_class.map(to_string),
            tagging: Some(tagging),
            ..Default::default()
        };

        Box::pin(async move { client.put_object(request).await }.instrument(info_span!("request")))
    }
}

fn build_request(
    req: PartitionInnerBuffer<Vec<u8>, Bytes>,
    time_format: String,
    extension: Option<String>,
    uuid: bool,
    compression: Compression,
    bucket: String,
    options: S3Options,
) -> Request {
    let (inner, key) = req.into_parts();

    // TODO: pull the seconds from the last event
    let filename = {
        let seconds = Utc::now().format(&time_format);

        if uuid {
            let uuid = Uuid::new_v4();
            format!("{}-{}", seconds, uuid.to_hyphenated())
        } else {
            seconds.to_string()
        }
    };

    let extension = extension.unwrap_or_else(|| compression.extension().into());
    let key = String::from_utf8_lossy(&key[..]).into_owned();
    let key = format!("{}{}.{}", key, filename, extension);

    debug!(
        message = "sending events.",
        bytes = &field::debug(inner.len()),
        bucket = &field::debug(&bucket),
        key = &field::debug(&key)
    );

    Request {
        body: inner,
        bucket,
        key,
        content_encoding: compression.content_encoding(),
        options,
    }
}

#[derive(Debug, Clone)]
struct Request {
    body: Vec<u8>,
    bucket: String,
    key: String,
    content_encoding: Option<&'static str>,
    options: S3Options,
}

impl Response for PutObjectOutput {}

#[derive(Debug, Clone)]
struct S3RetryLogic;

impl RetryLogic for S3RetryLogic {
    type Error = RusotoError<PutObjectError>;
    type Response = PutObjectOutput;

    fn is_retriable_error(&self, error: &Self::Error) -> bool {
        match error {
            RusotoError::HttpDispatch(_) => true,
            RusotoError::Unknown(res) if res.status.is_server_error() => true,
            _ => false,
        }
    }
}

fn encode_event(
    mut event: Event,
    key_prefix: &Template,
    encoding: &EncodingConfigWithDefault<Encoding>,
) -> Option<PartitionInnerBuffer<Vec<u8>, Bytes>> {
    let key = key_prefix
        .render_string(&event)
        .map_err(|missing_keys| {
            warn!(
                message = "Keys do not exist on the event. Dropping event.",
                ?missing_keys,
                rate_limit_secs = 30,
            );
        })
        .ok()?;

    encoding.apply_rules(&mut event);

    let log = event.into_log();
    let bytes = match encoding.codec() {
        Encoding::Ndjson => serde_json::to_vec(&log)
            .map(|mut b| {
                b.push(b'\n');
                b
            })
            .expect("Failed to encode event as json, this is a bug!"),
        Encoding::Text => {
            let mut bytes = log
                .get(&event::log_schema().message_key())
                .map(|v| v.as_bytes().to_vec())
                .unwrap_or_default();
            bytes.push(b'\n');
            bytes
        }
    };

    Some(PartitionInnerBuffer::new(bytes, key.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{self, Event};

    use std::collections::BTreeMap;

    #[test]
    fn s3_encode_event_text() {
        let message = "hello world".to_string();
        let batch_time_format = Template::try_from("date=%F").unwrap();
        let bytes = encode_event(
            message.clone().into(),
            &batch_time_format,
            &Encoding::Text.into(),
        )
        .unwrap();

        let encoded_message = message + "\n";
        let (bytes, _) = bytes.into_parts();
        assert_eq!(&bytes[..], encoded_message.as_bytes());
    }

    #[test]
    fn s3_encode_event_ndjson() {
        let message = "hello world".to_string();
        let mut event = Event::from(message.clone());
        event.as_mut_log().insert("key", "value");

        let batch_time_format = Template::try_from("date=%F").unwrap();
        let bytes = encode_event(event, &batch_time_format, &Encoding::Ndjson.into()).unwrap();

        let (bytes, _) = bytes.into_parts();
        let map: BTreeMap<String, String> = serde_json::from_slice(&bytes[..]).unwrap();

        assert_eq!(map[&event::log_schema().message_key().to_string()], message);
        assert_eq!(map["key"], "value".to_string());
    }

    #[test]
    fn s3_encode_event_with_removed_key() {
        let message = "hello world".to_string();
        let mut event = Event::from(message.clone());
        event.as_mut_log().insert("key", "value");

        let key_prefix = Template::try_from("{{ key }}").unwrap();

        let encoding_config = EncodingConfigWithDefault {
            codec: Encoding::Ndjson,
            except_fields: Some(vec!["key".into()]),
            ..Default::default()
        };

        let bytes = encode_event(event, &key_prefix, &encoding_config).unwrap();

        let (bytes, _) = bytes.into_parts();
        let map: BTreeMap<String, String> = serde_json::from_slice(&bytes[..]).unwrap();

        assert_eq!(map[&event::log_schema().message_key().to_string()], message);
        // assert_eq!(map["key"], "value".to_string());
    }

    #[test]
    fn s3_build_request() {
        let buf = PartitionInnerBuffer::new(vec![0u8; 10], Bytes::from("key/"));

        let req = build_request(
            buf.clone(),
            "date".into(),
            Some("ext".into()),
            false,
            Compression::None,
            "bucket".into(),
            S3Options::default(),
        );
        assert_eq!(req.key, "key/date.ext".to_string());

        let req = build_request(
            buf.clone(),
            "date".into(),
            None,
            false,
            Compression::None,
            "bucket".into(),
            S3Options::default(),
        );
        assert_eq!(req.key, "key/date.log".to_string());

        let req = build_request(
            buf.clone(),
            "date".into(),
            None,
            false,
            Compression::Gzip,
            "bucket".into(),
            S3Options::default(),
        );
        assert_eq!(req.key, "key/date.log.gz".to_string());

        let req = build_request(
            buf,
            "date".into(),
            None,
            true,
            Compression::Gzip,
            "bucket".into(),
            S3Options::default(),
        );
        assert_ne!(req.key, "key/date.log.gz".to_string());
    }
}

#[cfg(feature = "aws-s3-integration-tests")]
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{
        assert_downcast_matches,
        dns::Resolver,
        event::Event,
        region::RegionOrEndpoint,
        test_util::{random_lines_with_stream, random_string, runtime},
        topology::config::SinkContext,
    };
    use bytes05::{buf::BufExt, BytesMut};
    use flate2::read::GzDecoder;
    use futures::compat::Future01CompatExt;
    use futures::stream::{self, StreamExt};
    use futures01::Sink;
    use pretty_assertions::assert_eq;
    use rusoto_core::region::Region;
    use rusoto_s3::{S3Client, S3};
    use std::io::{BufRead, BufReader};

    const BUCKET: &str = "router-tests";

    #[test]
    fn s3_insert_message_into() {
        let mut rt = runtime();
        let cx = SinkContext::new_test();

        rt.block_on_std(async move {
            let config = config(1000000).await;
            let prefix = config.key_prefix.clone();
            let client = config.create_client(cx.resolver()).unwrap();
            let sink = config.new(client, cx).unwrap();

            let (lines, events) = random_lines_with_stream(100, 10);

            let _ = sink.send_all(events).compat().await.unwrap();

            let keys = get_keys(prefix.unwrap()).await;
            assert_eq!(keys.len(), 1);

            let key = keys[0].clone();
            assert!(key.ends_with(".log"));

            let obj = get_object(key).await;
            assert_eq!(obj.content_encoding, None);

            let response_lines = get_lines(obj).await;
            assert_eq!(lines, response_lines);
        })
    }

    #[test]
    fn s3_rotate_files_after_the_buffer_size_is_reached() {
        let mut rt = runtime();
        let cx = SinkContext::new_test();

        rt.block_on_std(async move {
            let config = S3SinkConfig {
                key_prefix: Some(format!("{}/{}", random_string(10), "{{i}}")),
                filename_time_format: Some("waitsforfullbatch".into()),
                filename_append_uuid: Some(false),
                ..config(1010).await
            };
            let prefix = config.key_prefix.clone();
            let client = config.create_client(cx.resolver()).unwrap();
            let sink = config.new(client, cx).unwrap();

            let (lines, _events) = random_lines_with_stream(100, 30);

            let events = lines.clone().into_iter().enumerate().map(|(i, line)| {
                let mut e = Event::from(line);
                let i = if i < 10 {
                    1
                } else if i < 20 {
                    2
                } else {
                    3
                };
                e.as_mut_log().insert("i", format!("{}", i));
                e
            });

            let _ = sink
                .send_all(futures01::stream::iter_ok(events))
                .compat()
                .await
                .unwrap();

            let keys = get_keys(prefix.unwrap()).await;
            assert_eq!(keys.len(), 3);

            let response_lines = stream::iter(keys)
                .fold(Vec::new(), |mut acc, key| async {
                    acc.push(get_lines(get_object(key).await).await);
                    acc
                })
                .await;

            assert_eq!(&lines[00..10], response_lines[0].as_slice());
            assert_eq!(&lines[10..20], response_lines[1].as_slice());
            assert_eq!(&lines[20..30], response_lines[2].as_slice());
        });
    }

    #[test]
    fn s3_gzip() {
        let mut rt = runtime();
        let cx = SinkContext::new_test();

        rt.block_on_std(async {
            let config = S3SinkConfig {
                compression: Compression::Gzip,
                filename_time_format: Some("%S%f".into()),
                ..config(10000).await
            };

            let prefix = config.key_prefix.clone();
            let client = config.create_client(cx.resolver()).unwrap();
            let sink = config.new(client, cx).unwrap();

            let (lines, events) = random_lines_with_stream(100, 500);

            let _ = sink.send_all(events).compat().await.unwrap();

            let keys = get_keys(prefix.unwrap()).await;
            assert_eq!(keys.len(), 6);

            let response_lines = stream::iter(keys).fold(Vec::new(), |mut acc, key| async {
                assert!(key.ends_with(".log.gz"));

                let obj = get_object(key).await;
                assert_eq!(obj.content_encoding, Some("gzip".to_string()));

                acc.append(&mut get_gzipped_lines(obj).await);
                acc
            });

            assert_eq!(lines, response_lines.await);
        });
    }

    #[test]
    fn s3_healthchecks() {
        let mut rt = runtime();
        let resolver = Resolver;

        rt.block_on_std(async move {
            let config = config(1).await;
            let client = config.create_client(resolver).unwrap();
            config.healthcheck(client).await.unwrap();
        });
    }

    #[test]
    fn s3_healthchecks_invalid_bucket() {
        let mut rt = runtime();
        let resolver = Resolver;

        rt.block_on_std(async move {
            let config = S3SinkConfig {
                bucket: "asdflkjadskdaadsfadf".to_string(),
                ..config(1).await
            };
            let client = config.create_client(resolver).unwrap();
            assert_downcast_matches!(
                config.healthcheck(client).await.unwrap_err(),
                HealthcheckError,
                HealthcheckError::UnknownBucket{ .. }
            );
        });
    }

    fn client() -> S3Client {
        let region = Region::Custom {
            name: "minio".to_owned(),
            endpoint: "http://localhost:4572".to_owned(),
        };

        use rusoto_core::HttpClient;
        use rusoto_credential::StaticProvider;

        let p = StaticProvider::new_minimal("test-access-key".into(), "test-secret-key".into());
        let d = HttpClient::new().unwrap();

        S3Client::new_with(d, p, region)
    }

    async fn config(batch_size: usize) -> S3SinkConfig {
        ensure_bucket(&client()).await;

        S3SinkConfig {
            key_prefix: Some(random_string(10) + "/date=%F/"),
            bucket: BUCKET.to_string(),
            compression: Compression::None,
            batch: BatchConfig {
                max_bytes: Some(batch_size),
                timeout_secs: Some(5),
                ..Default::default()
            },
            region: RegionOrEndpoint::with_endpoint("http://localhost:4572".to_owned()),
            ..Default::default()
        }
    }

    async fn ensure_bucket(client: &S3Client) {
        use rusoto_s3::{CreateBucketError, CreateBucketRequest};

        let req = CreateBucketRequest {
            bucket: BUCKET.to_string(),
            ..Default::default()
        };

        match client.create_bucket(req).await {
            Ok(_) | Err(RusotoError::Service(CreateBucketError::BucketAlreadyOwnedByYou(_))) => {}
            Err(e) => match e {
                RusotoError::Unknown(resp) => {
                    let body = String::from_utf8_lossy(&resp.body[..]);
                    panic!("Couldn't create bucket: {:?}; Body {}", resp, body);
                }
                _ => panic!("Couldn't create bucket: {}", e),
            },
        }
    }

    async fn get_keys(prefix: String) -> Vec<String> {
        let prefix = prefix.split("/").into_iter().next().unwrap().to_string();

        let list_res = client()
            .list_objects_v2(rusoto_s3::ListObjectsV2Request {
                bucket: BUCKET.to_string(),
                prefix: Some(prefix),
                ..Default::default()
            })
            .await
            .unwrap();

        list_res
            .contents
            .unwrap()
            .into_iter()
            .map(|obj| obj.key.unwrap())
            .collect()
    }

    async fn get_object(key: String) -> rusoto_s3::GetObjectOutput {
        client()
            .get_object(rusoto_s3::GetObjectRequest {
                bucket: BUCKET.to_string(),
                key,
                ..Default::default()
            })
            .await
            .unwrap()
    }

    async fn get_lines(obj: rusoto_s3::GetObjectOutput) -> Vec<String> {
        let body = get_object_output_body(obj).await;
        let buf_read = BufReader::new(body);
        buf_read.lines().map(|l| l.unwrap()).collect()
    }

    async fn get_gzipped_lines(obj: rusoto_s3::GetObjectOutput) -> Vec<String> {
        let body = get_object_output_body(obj).await;
        let buf_read = BufReader::new(GzDecoder::new(body));
        buf_read.lines().map(|l| l.unwrap()).collect()
    }

    async fn get_object_output_body(obj: rusoto_s3::GetObjectOutput) -> impl std::io::Read {
        let bytes = obj
            .body
            .unwrap()
            .fold(BytesMut::new(), |mut store, bytes| async move {
                store.extend_from_slice(&bytes.unwrap());
                store
            })
            .await;
        bytes.freeze().reader()
    }
}
