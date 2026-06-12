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
POST /api/auth/agent-tokens
```

The stored token can be overridden per invocation with environment variables, checked
in this order:

```text
CPU_MODE_TOKEN       token value
CPU_MODE_TOKEN_FILE  path to a file containing the token
```

## Agent tokens

To attribute and isolate submissions made by an AI agent, mint a token scoped to an
agent name (requires a regular `cpu-mode auth login` first):

```bash
cpu-mode auth create-agent-token --agent claude-fable-5
```

Agent names are limited to 64 bytes of `[A-Za-z0-9._-]`. The scope is enforced
server-side:

- Solutions submitted with the token are stamped with the agent name (visible as
  `agent` in API responses and in `cpu-mode solutions show`). The label always comes
  from the token and cannot be set in the submission request.
- The token can only read solution source, compiler options, and job profiles from
  the same agent — other solutions, including public ones, stay hidden.
- The token cannot mint further tokens.

To hand the token to an agent without letting it tamper with the scope, store it in a
file the agent process cannot write and point the agent's environment at it:

```bash
cpu-mode auth create-agent-token --agent claude-fable-5   # prints the token
sudo tee /etc/cpu-mode/agent-token > /dev/null            # paste token
sudo chmod 444 /etc/cpu-mode/agent-token
export CPU_MODE_TOKEN_FILE=/etc/cpu-mode/agent-token
```

Make sure the agent's environment has no regular (unscoped) credentials, i.e. no
`cpu-mode auth login` config in its home directory.

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

Read a leaderboard:

```bash
cpu-mode leaderboard counting_bytes --system raptor_cove_p
```

Read the all-system geomean slowdown leaderboard:

```bash
cpu-mode leaderboard counting_bytes --all-systems
```

Public solutions that are not ranked because the same user has a faster solution
can still appear with a blank rank.

Submit a Rust solution and wait for all system jobs:

```bash
cpu-mode submit counting_bytes --lang rust --file solution.rs --wait
```

Submit C++ with GCC instead of the default Clang compiler:

```bash
cpu-mode submit counting_bytes --lang cpp --compiler gcc_cpp --file solution.cpp --wait
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

Show a job's top-down analysis:

```bash
cpu-mode jobs top-down job_...
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
