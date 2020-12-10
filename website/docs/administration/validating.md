---
last_modified_on: "2020-07-13"
title: Validating
description: How to validation Vector's configuration to ensure it is error free before applying it.
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';

Vector provides a subcommand `validate` which checks the validity of any number
of configuration files and then exits:

<Tabs
  block={true}
  defaultValue="all"
  values={[
    { label: 'All Checks', value: 'all', },
    { label: 'Config Only', value: 'config', },
  ]
}>
<TabItem value="all">

```bash
vector validate /etc/vector/vector.toml
```

</TabItem>
<TabItem value="config">

```bash
vector validate --no-environment --no-topology /etc/vector/*.toml
```

</TabItem>
</Tabs>

The validate subcommand checks the correctness of fields for components defined
within a configuration file, including:

1. All [sources][docs.sources], [transforms][docs.transforms], and
[sinks][docs.sinks] include all non-optional fields.
2. All fields are of the proper [type][docs.configuration#types].

The following group of checks verifies that the configuration file contains a valid topology,
disabled with flags such as `--no-topology`, expanding the above checks with the following:

3. At least one [source][docs.sources] is defined.
4. At least one [sink][docs.sinks] is defined.
5. All `inputs` values contain at least one value (cannot be empty).
6. All `inputs` values reference valid and upstream [source][docs.sources] or
[transform][docs.transforms] components.

The following group of checks require the runtime environment to pass successfully,
disabled with flags such as `--no-environment`, expanding the above checks with the following:

7. All components are capable of running (data directories exist, are writable, etc).
8. All [sinks][docs.sinks] are able to connect to their targets.

If validation fails, Vector will exit with a `78`, and if validation succeeds
Vector will exit with a `0`.

To see other customization options for the `validate` subcommand run
`vector validate --help`.

[docs.configuration#types]: /docs/setup/configuration/#types
[docs.sinks]: /docs/reference/sinks/
[docs.sources]: /docs/reference/sources/
[docs.transforms]: /docs/reference/transforms/
