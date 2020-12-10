---
last_modified_on: "2020-07-15"
$schema: "/.meta/.schemas/highlights.json"
title: "Kafka components support SASL"
description: "Vector has a new way to authenticate with Kafka!"
author_github: "https://github.com/hoverbear"
hide_on_release_notes: false
pr_numbers: [2897]
release: "0.10.0"
tags: ["type: new feature","domain: sinks","sink: kafka"]
---

import Alert from '@site/src/components/Alert';

The Kafka source and sink now support [SASL authentication][urls.kafka_sasl].

You can review the option in the [component docs][urls.vector_sink_kafka_sasl].

```diff title="vector.toml"
  [sources.source0]
    type = "kafka" # required
    inputs = ["..."] # required
    bootstrap_servers = "10.14.22.123:9092,10.14.23.332:9092" # required
    group_id = "consumer-group-name" # required
    key_field = "message_key" # optional, no default
    topics = ["^(prefix1|prefix2)-.+", "topic-1", "topic-2"] # required
+   sasl.enabled = true # optional, default false
+   sasl.mechanism = "SCRAM-SHA-512" # optional, no default
+   sasl.password = "password" # optional, no default
+   sasl.username = "username" # optional, no default
```

<Alert type="warning">

This feature isn't yet supported on Windows.

</Alert>

[urls.kafka_sasl]: https://docs.confluent.io/current/kafka/authentication_sasl/index.html
[urls.vector_sink_kafka_sasl]: https://vector.dev/docs/reference/sources/kafka/#sasl
