---
last_modified_on: "2020-04-13"
$schema: "/.meta/.schemas/highlights.json"
title: "Compression Now Available In The Kafka Sink"
description: "Improve throughput by compressing data before writing it to Kafka"
author_github: "https://github.com/binarylogic"
pr_numbers: [1969]
release: "0.9.0"
hide_on_release_notes: true
tags: ["type: new feature", "domain: sinks", "sink: kafka"]
---

Compression for Vector's [`kafka` sink][docs.sinks.kafka] is now available.
Before we take credit for this feature, Vector uses
[`librdkafka`][urls.librdkafka] under the hood, and to maintain consistency
we just mapped the appropriate options. In addition, we added a
[new `librdkafka_options`][docs.sinks.kafka#librdkafka_options] that enables
transparent pass-through of [`librdkafka`'s options][urls.librdkafka_config].

[docs.sinks.kafka#librdkafka_options]: /docs/reference/sinks/kafka/#librdkafka_options
[docs.sinks.kafka]: /docs/reference/sinks/kafka/
[urls.librdkafka]: https://github.com/edenhill/librdkafka
[urls.librdkafka_config]: https://github.com/edenhill/librdkafka/blob/master/CONFIGURATION.md
