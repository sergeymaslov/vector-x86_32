---
last_modified_on: "2020-04-19"
$schema: "/.meta/.schemas/highlights.json"
title: "Introducing Vector's Global Log Schema"
description: "Set defaults for Vector's common log key names"
author_github: "https://github.com/binarylogic"
pr_numbers: [1769, 1795]
release: "0.8.0"
hide_on_release_notes: false
tags: ["type: new feature", "domain: config"]
---

Vector does not require a rigid schema for it's [`log`
events][docs.data-model.log]. You are welcome to use any field names you like,
such as the `timestamp`, `message`, and `host`. Until recently, the
default names of these fields were not easily customizable. You either had to
set these names within the [source][docs.sources] itself, or rename these fields
using the [`rename_fields` transform][docs.transforms.rename_fields]. While this
works, it's combersome and is not obvious to anyone reading your Vector
configuration file. Enter Vector's new [global log
schema][docs.global-options#log_schema]. These new options allow you to change
the default names for the [`message_key`][docs.global-options#message_key],
[`host_key`][docs.global-options#host_key],
[`timestamp_key`][docs.global-options#host_key], and more:

```toml title="vector.toml"
[log_schema]
  host_key = "host" # default
  message_key = "message" # default
  timestamp_key = "timestamp" # default
```

Why is this useful?

1. Many Vector users already have a schema in-place and this makes it easy for
   Vector to adopt that schema.
2. Components often times need to coordinate. For example, the
   [`host_key`][docs.global-options#host_key] is used in a variety of
   [sinks][docs.sinks] to ensure that Vector's internal "host" field is mapped
   to the downstream service's "host" field.

[docs.data-model.log]: /docs/about/data-model/log/
[docs.global-options#host_key]: /docs/reference/global-options/#host_key
[docs.global-options#log_schema]: /docs/reference/global-options/#log_schema
[docs.global-options#message_key]: /docs/reference/global-options/#message_key
[docs.sinks]: /docs/reference/sinks/
[docs.sources]: /docs/reference/sources/
[docs.transforms.rename_fields]: /docs/reference/transforms/rename_fields/
