# cpu-mode

Command-line client for [cpu.mode](https://cpu.mattstuchlik.com), a benchmark judge for tiny systems-code challenges.

This repository is intentionally separate from the private server and judge repo. The CLI talks only to the public HTTP API.

## Install

From crates.io:

```bash
cargo install cpu-mode
```

From source:

```bash
cargo install --git https://github.com/s7nfo/cpu-mode-cli
```

Upgrade an existing install:

```bash
cargo install cpu-mode --force
```

## Login

```bash
cpu-mode auth login
```

The CLI uses the cpu.mode headless login flow. It prints a GitHub verification URL and code, waits for approval, then stores a cpu.mode API token locally.

The server-side auth endpoints are:

```text
POST /auth/cli/start
POST /auth/cli/poll
GET  /auth/session
```

## Examples

List challenges:

```bash
cpu-mode challenges list
```

Print the raw API response:

```bash
cpu-mode --raw challenges list
```

Show challenge metadata:

```bash
cpu-mode challenges show counting_bytes
```

Submit a Rust solution and wait for all system jobs:

```bash
cpu-mode submit counting_bytes --lang rust --file solution.rs --wait
```

Inspect a job:

```bash
cpu-mode jobs show job_...
```

Show queued jobs first, then newest jobs by queue time:

```bash
cpu-mode jobs queue
```

Download a job profile:

```bash
cpu-mode jobs profile job_... --output profile.txt
```

Show a solution and make it public:

```bash
cpu-mode solutions show sol_...
cpu-mode solutions publish sol_...
```

Make it private again:

```bash
cpu-mode solutions unpublish sol_...
```

Show a user submission history:

```bash
cpu-mode users jobs github:34958324 --challenge counting_bytes --limit 20
```

Command output is human-readable by default. Pass global `--raw` when an agent or script
needs the exact JSON API response.
