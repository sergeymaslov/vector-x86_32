---
last_modified_on: "2020-07-13"
$schema: "/.meta/.schemas/highlights.json"
title: "Beautification of the validate command"
description: "A little polish on a useful feature."
author_github: "https://github.com/hoverbear"
hide_on_release_notes: false
pr_numbers: [2622]
release: "0.10.0"
tags: ["type: enhancement", "domain: ux"]
---

We gave `vector validate` some touching up to make it look better, and feel nicer to use. This was heavily inspired by
the fantastic `linkerd validate` command.

Take a gander at the new output on a good valid configuration:

```bash
vic@sticky-keyboard-macbook:/git/timberio/vector$ vector validate test.toml
√ Loaded "test.toml"
√ Configuration topology
√ Component configuration
√ Health check `sink0`
-------------------------
                Validated
```

Now an invalid:

```bash
vic@sticky-keyboard-macbook:/git/timberio/vector$ vector validate test.toml
Failed to parse "test.toml"
---------------------------
x missing field `type` for key `sinks.sink1`
```


