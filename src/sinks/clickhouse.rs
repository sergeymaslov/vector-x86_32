use crate::{
    event::Event,
    sinks::util::{
        encoding::{EncodingConfigWithDefault, EncodingConfiguration},
        http::{Auth, BatchedHttpSink, HttpClient, HttpRetryLogic, HttpSink},
        retries2::{RetryAction, RetryLogic},
        service2::TowerRequestConfig,
        BatchConfig, BatchSettings, Buffer, Compression,
    },
    tls::{TlsOptions, TlsSettings},
    topology::config::{DataType, SinkConfig, SinkContext, SinkDescription},
};
use futures::{FutureExt, TryFutureExt};
use futures01::Sink;
use http::{Request, StatusCode, Uri};
use hyper::Body;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use snafu::ResultExt;

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ClickhouseConfig {
    pub host: String,
    pub table: String,
    pub database: Option<String>,
    #[serde(default = "Compression::default_gzip")]
    pub compression: Compression,
    #[serde(
        skip_serializing_if = "crate::serde::skip_serializing_if_default",
        default
    )]
    pub encoding: EncodingConfigWithDefault<Encoding>,
    #[serde(default)]
    pub batch: BatchConfig,
    pub auth: Option<Auth>,
    #[serde(default)]
    pub request: TowerRequestConfig,
    pub tls: Option<TlsOptions>,
}

lazy_static! {
    static ref REQUEST_DEFAULTS: TowerRequestConfig = TowerRequestConfig {
        ..Default::default()
    };
}

inventory::submit! {
    SinkDescription::new::<ClickhouseConfig>("clickhouse")
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone, Derivative)]
#[serde(rename_all = "snake_case")]
#[derivative(Default)]
pub enum Encoding {
    #[derivative(Default)]
    Default,
}

#[typetag::serde(name = "clickhouse")]
impl SinkConfig for ClickhouseConfig {
    fn build(&self, cx: SinkContext) -> crate::Result<(super::RouterSink, super::Healthcheck)> {
        let batch = self.batch.use_size_as_bytes()?.get_settings_or_default(
            BatchSettings::default()
                .bytes(bytesize::mib(10u64))
                .timeout(1),
        );
        let request = self.request.unwrap_with(&REQUEST_DEFAULTS);
        let tls_settings = TlsSettings::from_options(&self.tls)?;
        let client = HttpClient::new(cx.resolver(), tls_settings)?;

        let sink = BatchedHttpSink::new(
            self.clone(),
            Buffer::new(batch.size, self.compression),
            request,
            batch.timeout,
            client.clone(),
            cx.acker(),
        )
        .sink_map_err(|e| error!("Fatal clickhouse sink error: {}", e));

        let healthcheck = healthcheck(client, self.clone()).boxed().compat();

        Ok((Box::new(sink), Box::new(healthcheck)))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn sink_type(&self) -> &'static str {
        "clickhouse"
    }
}

#[async_trait::async_trait]
impl HttpSink for ClickhouseConfig {
    type Input = Vec<u8>;
    type Output = Vec<u8>;

    fn encode_event(&self, mut event: Event) -> Option<Self::Input> {
        self.encoding.apply_rules(&mut event);

        let mut body =
            serde_json::to_vec(&event.as_log().all_fields()).expect("Events should be valid json!");
        body.push(b'\n');

        Some(body)
    }

    async fn build_request(&self, events: Self::Output) -> crate::Result<http::Request<Vec<u8>>> {
        let database = if let Some(database) = &self.database {
            database.as_str()
        } else {
            "default"
        };

        let uri = encode_uri(&self.host, database, &self.table).expect("Unable to encode uri");

        let mut builder = Request::post(&uri).header("Content-Type", "application/x-ndjson");

        if let Some(ce) = self.compression.content_encoding() {
            builder = builder.header("Content-Encoding", ce);
        }

        let mut request = builder.body(events).unwrap();

        if let Some(auth) = &self.auth {
            auth.apply(&mut request);
        }

        Ok(request)
    }
}

async fn healthcheck(mut client: HttpClient, config: ClickhouseConfig) -> crate::Result<()> {
    // TODO: check if table exists?
    let uri = format!("{}/?query=SELECT%201", config.host);
    let mut request = Request::get(uri).body(Body::empty()).unwrap();

    if let Some(auth) = &config.auth {
        auth.apply(&mut request);
    }

    let response = client.send(request).await?;

    match response.status() {
        StatusCode::OK => Ok(()),
        status => Err(super::HealthcheckError::UnexpectedStatus { status }.into()),
    }
}

fn encode_uri(host: &str, database: &str, table: &str) -> crate::Result<Uri> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(
            "query",
            format!(
                "INSERT INTO \"{}\".\"{}\" FORMAT JSONEachRow",
                database,
                table.replace("\"", "\\\"")
            )
            .as_str(),
        )
        .finish();

    let url = if host.ends_with('/') {
        format!("{}?{}", host, query)
    } else {
        format!("{}/?{}", host, query)
    };

    Ok(url.parse::<Uri>().context(super::UriParseError)?)
}

#[derive(Clone)]
struct ClickhouseRetryLogic {
    inner: HttpRetryLogic,
}

impl RetryLogic for ClickhouseRetryLogic {
    type Response = http::Response<bytes05::Bytes>;
    type Error = hyper::Error;

    fn is_retriable_error(&self, error: &Self::Error) -> bool {
        self.inner.is_retriable_error(error)
    }

    fn should_retry_response(&self, response: &Self::Response) -> RetryAction {
        match response.status() {
            StatusCode::INTERNAL_SERVER_ERROR => {
                let body = response.body();

                // Currently, clickhouse returns 500's incorrect data and type mismatch errors.
                // This attempts to check if the body starts with `Code: {code_num}` and to not
                // retry those errors.
                //
                // Reference: https://github.com/timberio/vector/pull/693#issuecomment-517332654
                // Error code definitions: https://github.com/ClickHouse/ClickHouse/blob/master/dbms/src/Common/ErrorCodes.cpp
                if body.starts_with(b"Code: 117") {
                    RetryAction::DontRetry("incorrect data".into())
                } else if body.starts_with(b"Code: 53") {
                    RetryAction::DontRetry("type mismatch".into())
                } else {
                    RetryAction::Retry(String::from_utf8_lossy(body).to_string())
                }
            }
            _ => self.inner.should_retry_response(response),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_valid() {
        let uri = encode_uri("http://localhost:80", "my_database", "my_table").unwrap();
        assert_eq!(uri, "http://localhost:80/?query=INSERT+INTO+%22my_database%22.%22my_table%22+FORMAT+JSONEachRow");

        let uri = encode_uri("http://localhost:80", "my_database", "my_\"table\"").unwrap();
        assert_eq!(uri, "http://localhost:80/?query=INSERT+INTO+%22my_database%22.%22my_%5C%22table%5C%22%22+FORMAT+JSONEachRow");
    }

    #[test]
    fn encode_invalid() {
        encode_uri("localhost:80", "my_database", "my_table").unwrap_err();
    }
}

#[cfg(test)]
#[cfg(feature = "clickhouse-integration-tests")]
mod integration_tests {
    use super::*;
    use crate::{
        event,
        event::Event,
        sinks::util::encoding::TimestampFormat,
        test_util::{random_string, runtime},
        topology::config::{SinkConfig, SinkContext},
    };
    use futures::compat::Future01CompatExt;
    use futures01::Sink;
    use serde_json::Value;
    use std::time::Duration;
    use tokio01::util::FutureExt;

    #[test]
    fn insert_events() {
        crate::test_util::trace_init();

        let mut rt = runtime();

        rt.block_on_std(async move {
            let table = gen_table();
            let host = String::from("http://localhost:8123");

            let config = ClickhouseConfig {
                host: host.clone(),
                table: table.clone(),
                compression: Compression::None,
                batch: BatchConfig {
                    max_events: Some(1),
                    ..Default::default()
                },
                request: TowerRequestConfig {
                    retry_attempts: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            };

            let client = ClickhouseClient::new(host);
            client
                .create_table(&table, "host String, timestamp String, message String")
                .await;

            let (sink, _hc) = config.build(SinkContext::new_test()).unwrap();

            let mut input_event = Event::from("raw log line");
            input_event.as_mut_log().insert("host", "example.com");

            sink.send(input_event.clone()).compat().await.unwrap();

            let output = client.select_all(&table).await;
            assert_eq!(1, output.rows);

            let expected = serde_json::to_value(input_event.into_log().all_fields()).unwrap();
            assert_eq!(expected, output.data[0]);
        });
    }

    #[test]
    fn insert_events_unix_timestamps() {
        crate::test_util::trace_init();

        let mut rt = runtime();

        rt.block_on_std(async move {
            let table = gen_table();
            let host = String::from("http://localhost:8123");
            let mut encoding = EncodingConfigWithDefault::default();
            encoding.timestamp_format = Some(TimestampFormat::Unix);

            let config = ClickhouseConfig {
                host: host.clone(),
                table: table.clone(),
                compression: Compression::None,
                encoding,
                batch: BatchConfig {
                    max_events: Some(1),
                    ..Default::default()
                },
                request: TowerRequestConfig {
                    retry_attempts: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            };

            let client = ClickhouseClient::new(host);
            client
                .create_table(
                    &table,
                    "host String, timestamp DateTime('UTC'), message String",
                )
                .await;

            let (sink, _hc) = config.build(SinkContext::new_test()).unwrap();

            let mut input_event = Event::from("raw log line");
            input_event.as_mut_log().insert("host", "example.com");

            sink.send(input_event.clone()).compat().await.unwrap();

            let output = client.select_all(&table).await;
            assert_eq!(1, output.rows);

            let exp_event = input_event.as_mut_log();
            exp_event.insert(
                event::log_schema().timestamp_key().clone(),
                format!(
                    "{}",
                    exp_event
                        .get(&event::log_schema().timestamp_key())
                        .unwrap()
                        .as_timestamp()
                        .unwrap()
                        .format("%Y-%m-%d %H:%M:%S")
                ),
            );

            let expected = serde_json::to_value(exp_event.all_fields()).unwrap();
            assert_eq!(expected, output.data[0]);
        });
    }

    #[test]
    fn insert_events_unix_timestamps_toml_config() {
        crate::test_util::trace_init();

        let mut rt = runtime();

        rt.block_on_std(async move {
            let table = gen_table();
            let host = String::from("http://localhost:8123");

            let config: ClickhouseConfig = toml::from_str(&format!(
                r#"
host = "{}"
table = "{}"
compression = "none"
[request]
retry_attempts = 1
[batch]
max_events = 1
[encoding]
timestamp_format = "unix""#,
                host, table
            ))
            .unwrap();

            let client = ClickhouseClient::new(host);
            client
                .create_table(
                    &table,
                    "host String, timestamp DateTime('UTC'), message String",
                )
                .await;

            let (sink, _hc) = config.build(SinkContext::new_test()).unwrap();

            let mut input_event = Event::from("raw log line");
            input_event.as_mut_log().insert("host", "example.com");

            sink.send(input_event.clone()).compat().await.unwrap();

            let output = client.select_all(&table).await;
            assert_eq!(1, output.rows);

            let exp_event = input_event.as_mut_log();
            exp_event.insert(
                event::log_schema().timestamp_key().clone(),
                format!(
                    "{}",
                    exp_event
                        .get(&event::log_schema().timestamp_key())
                        .unwrap()
                        .as_timestamp()
                        .unwrap()
                        .format("%Y-%m-%d %H:%M:%S")
                ),
            );

            let expected = serde_json::to_value(exp_event.all_fields()).unwrap();
            assert_eq!(expected, output.data[0]);
        });
    }

    #[test]
    fn no_retry_on_incorrect_data() {
        crate::test_util::trace_init();

        let mut rt = runtime();

        rt.block_on_std(async move {
            let table = gen_table();
            let host = String::from("http://localhost:8123");

            let config = ClickhouseConfig {
                host: host.clone(),
                table: table.clone(),
                compression: Compression::None,
                batch: BatchConfig {
                    max_events: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            };

            let client = ClickhouseClient::new(host);
            // the event contains a message field, but its being omited to
            // fail the request.
            client
                .create_table(&table, "host String, timestamp String")
                .await;

            let (sink, _hc) = config.build(SinkContext::new_test()).unwrap();

            let mut input_event = Event::from("raw log line");
            input_event.as_mut_log().insert("host", "example.com");

            // Retries should go on forever, so if we are retrying incorrectly
            // this timeout should trigger.
            sink.send(input_event.clone())
                .timeout(Duration::from_secs(5))
                .compat()
                .await
                .unwrap();
        });
    }

    struct ClickhouseClient {
        host: String,
        client: reqwest::Client,
    }

    impl ClickhouseClient {
        fn new(host: String) -> Self {
            ClickhouseClient {
                host,
                client: reqwest::Client::new(),
            }
        }

        async fn create_table(&self, table: &str, schema: &str) {
            let response = self
                .client
                .post(&self.host)
                //
                .body(format!(
                    "CREATE TABLE {}
                     ({})
                     ENGINE = MergeTree()
                     ORDER BY (host, timestamp);",
                    table, schema
                ))
                .send()
                .await
                .unwrap();

            if !response.status().is_success() {
                panic!("create table failed: {}", response.text().await.unwrap())
            }
        }

        async fn select_all(&self, table: &str) -> QueryResponse {
            let response = self
                .client
                .post(&self.host)
                .body(format!("SELECT * FROM {} FORMAT JSON", table))
                .send()
                .await
                .unwrap();

            if !response.status().is_success() {
                panic!("select all failed: {}", response.text().await.unwrap())
            } else {
                let text = response.text().await.unwrap();
                match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(_) => panic!("json failed: {:?}", text),
                }
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct QueryResponse {
        data: Vec<Value>,
        meta: Vec<Value>,
        rows: usize,
        statistics: Stats,
    }

    #[derive(Debug, Deserialize)]
    struct Stats {
        bytes_read: usize,
        elapsed: f64,
        rows_read: usize,
    }

    fn gen_table() -> String {
        format!("test_{}", random_string(10).to_lowercase())
    }
}
