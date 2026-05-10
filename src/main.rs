use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::sleep;

const DEFAULT_BASE_URL: &str = "https://cpu.mattstuchlik.com";
const USER_AGENT: &str = concat!("cpu-mode-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Parser)]
#[command(name = "cpu-mode")]
#[command(about = "Command-line client for cpu.mode")]
struct Cli {
    #[arg(long, global = true, env = "CPU_MODE_BASE_URL", default_value = DEFAULT_BASE_URL)]
    base_url: String,

    #[arg(long, global = true, env = "CPU_MODE_TOKEN")]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Auth(AuthArgs),
    Challenges(ChallengesArgs),
    Systems(SystemsArgs),
    Leaderboard(LeaderboardArgs),
    Submit(SubmitArgs),
    Jobs(JobsArgs),
    Users(UsersArgs),
    Solutions(SolutionsArgs),
}

#[derive(Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    Login(AuthLoginArgs),
    Status,
    Logout,
}

#[derive(Args)]
struct AuthLoginArgs {
    #[arg(long)]
    no_store: bool,
}

#[derive(Args)]
struct ChallengesArgs {
    #[command(subcommand)]
    command: ChallengesCommand,
}

#[derive(Subcommand)]
enum ChallengesCommand {
    List,
    Show { challenge_id: String },
}

#[derive(Args)]
struct SystemsArgs {
    #[command(subcommand)]
    command: SystemsCommand,
}

#[derive(Subcommand)]
enum SystemsCommand {
    List,
}

#[derive(Args)]
struct LeaderboardArgs {
    challenge_id: String,

    #[arg(long)]
    system: Option<String>,
}

#[derive(Args)]
struct SubmitArgs {
    challenge_id: String,

    #[arg(long, value_parser = ["rust", "cpp"])]
    lang: String,

    #[arg(long)]
    file: PathBuf,

    #[arg(long)]
    compiler_options: Option<String>,

    #[arg(long)]
    wait: bool,

    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,
}

#[derive(Args)]
struct JobsArgs {
    #[command(subcommand)]
    command: JobsCommand,
}

#[derive(Subcommand)]
enum JobsCommand {
    Show {
        job_id: String,
    },
    Watch {
        job_id: String,

        #[arg(long, default_value_t = 1000)]
        poll_interval_ms: u64,
    },
    Queue {
        #[arg(long, default_value_t = 50)]
        limit: usize,

        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Args)]
struct UsersArgs {
    #[command(subcommand)]
    command: UsersCommand,
}

#[derive(Subcommand)]
enum UsersCommand {
    Jobs {
        user_id: String,

        #[arg(long)]
        challenge: Option<String>,

        #[arg(long, default_value_t = 20)]
        limit: usize,

        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Args)]
struct SolutionsArgs {
    #[command(subcommand)]
    command: SolutionsCommand,
}

#[derive(Subcommand)]
enum SolutionsCommand {
    Show {
        solution_id: String,
    },
    Jobs {
        solution_id: String,

        #[arg(long, default_value_t = 20)]
        limit: usize,

        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct StoredConfig {
    token: Option<String>,
}

struct ConfigStore {
    path: PathBuf,
    config: StoredConfig,
}

impl ConfigStore {
    fn load() -> Result<Self> {
        let path = config_path()?;
        let config = match fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents)
                .with_context(|| format!("parse config {}", path.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => StoredConfig::default(),
            Err(err) => return Err(err).with_context(|| format!("read config {}", path.display())),
        };
        Ok(Self { path, config })
    }

    fn token(&self) -> Option<&str> {
        self.config.token.as_deref()
    }

    fn set_token(&mut self, token: String) -> Result<()> {
        self.config.token = Some(token);
        self.save()
    }

    fn clear_token(&mut self) -> Result<()> {
        self.config.token = None;
        self.save()
    }

    fn save(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent: {}", self.path.display()))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
        let contents = toml::to_string_pretty(&self.config).context("serialize config")?;
        write_private_file(&self.path, contents.as_bytes())
            .with_context(|| format!("write config {}", self.path.display()))
    }
}

fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CPU_MODE_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let dirs = ProjectDirs::from("com", "cpu-mode", "cpu-mode")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.config_dir().join("config.toml"))
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(0o600);
    std::io::Write::write_all(&mut options.open(path)?, contents)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    fs::write(path, contents)
}

struct ApiClient {
    http: Client,
    base_url: String,
    token: Option<String>,
}

impl ApiClient {
    fn new(base_url: String, token: Option<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, &[], Some(body)).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let mut url = self.url(path)?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }

        let mut request = self
            .http
            .request(method, url)
            .header("user-agent", USER_AGENT);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.context("send request")?;
        let status = response.status();
        let text = response.text().await.context("read response body")?;
        if !status.is_success() {
            bail!("HTTP {status}: {text}");
        }
        parse_json_body(&text)
    }

    fn url(&self, path: &str) -> Result<Url> {
        Url::parse(&format!("{}{}", self.base_url, path))
            .with_context(|| format!("build URL for path {path}"))
    }
}

fn parse_json_body(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(text).with_context(|| format!("parse JSON response: {text}"))
    }
}

#[derive(Deserialize)]
struct AuthStartResponse {
    login_id: String,
    verification_uri: String,
    user_code: String,
    interval: Option<u64>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct AuthPollResponse {
    status: String,
    token: Option<String>,
    user: Option<Value>,
    interval: Option<u64>,
    message: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut store = ConfigStore::load()?;
    let token = cli
        .token
        .clone()
        .or_else(|| store.token().map(str::to_string));
    let client = ApiClient::new(cli.base_url, token);

    match cli.command {
        Command::Auth(args) => handle_auth(args, &client, &mut store).await,
        Command::Challenges(args) => handle_challenges(args, &client).await,
        Command::Systems(args) => handle_systems(args, &client).await,
        Command::Leaderboard(args) => handle_leaderboard(args, &client).await,
        Command::Submit(args) => handle_submit(args, &client).await,
        Command::Jobs(args) => handle_jobs(args, &client).await,
        Command::Users(args) => handle_users(args, &client).await,
        Command::Solutions(args) => handle_solutions(args, &client).await,
    }
}

async fn handle_auth(args: AuthArgs, client: &ApiClient, store: &mut ConfigStore) -> Result<()> {
    match args.command {
        AuthCommand::Login(args) => auth_login(args, client, store).await,
        AuthCommand::Status => {
            let session = client.get("/auth/session", &[]).await?;
            print_json(&session)
        }
        AuthCommand::Logout => {
            store.clear_token()?;
            print_json(&json!({"ok": true, "message": "local token removed"}))
        }
    }
}

async fn auth_login(
    args: AuthLoginArgs,
    client: &ApiClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let start: AuthStartResponse =
        serde_json::from_value(client.post("/auth/cli/start", json!({})).await?)
            .context("decode auth start response")?;
    eprintln!(
        "Open {} and enter code {}",
        start.verification_uri, start.user_code
    );
    if let Some(expires_in) = start.expires_in {
        eprintln!("This code expires in {expires_in} seconds.");
    }

    let mut interval = start.interval.unwrap_or(5).max(1);
    loop {
        sleep(Duration::from_secs(interval)).await;
        let poll: AuthPollResponse = serde_json::from_value(
            client
                .post("/auth/cli/poll", json!({ "login_id": start.login_id }))
                .await?,
        )
        .context("decode auth poll response")?;

        match poll.status.as_str() {
            "authorized" => {
                let token = poll
                    .token
                    .ok_or_else(|| anyhow!("authorized response did not include token"))?;
                if !args.no_store {
                    store.set_token(token.clone())?;
                }
                return print_json(&json!({
                    "authenticated": true,
                    "stored": !args.no_store,
                    "user": poll.user,
                    "token": if args.no_store { Some(token) } else { None },
                }));
            }
            "pending" => {}
            "slow_down" => {
                interval += 5;
            }
            "expired" => bail!(
                "{}",
                poll.message.unwrap_or_else(|| "login expired".to_string())
            ),
            "denied" => bail!(
                "{}",
                poll.message.unwrap_or_else(|| "login denied".to_string())
            ),
            other => bail!("unknown auth status {other}"),
        }
        if let Some(next_interval) = poll.interval {
            interval = next_interval.max(1);
        }
    }
}

async fn handle_challenges(args: ChallengesArgs, client: &ApiClient) -> Result<()> {
    let value = match args.command {
        ChallengesCommand::List => client.get("/api/challenges", &[]).await?,
        ChallengesCommand::Show { challenge_id } => {
            client
                .get(&format!("/api/challenges/{}", enc(&challenge_id)), &[])
                .await?
        }
    };
    print_json(&value)
}

async fn handle_systems(args: SystemsArgs, client: &ApiClient) -> Result<()> {
    match args.command {
        SystemsCommand::List => print_json(&client.get("/api/systems", &[]).await?),
    }
}

async fn handle_leaderboard(args: LeaderboardArgs, client: &ApiClient) -> Result<()> {
    let mut query = Vec::new();
    if let Some(system) = args.system {
        query.push(("system_id", system));
    }
    let value = client
        .get(
            &format!("/api/challenges/{}/leaderboard", enc(&args.challenge_id)),
            &query,
        )
        .await?;
    print_json(&value)
}

async fn handle_submit(args: SubmitArgs, client: &ApiClient) -> Result<()> {
    let source = read_source(&args.file)?;
    let mut body = json!({
        "language": args.lang,
        "source": source,
    });
    if let Some(options) = args.compiler_options {
        body["compiler_options"] = Value::String(options);
    }

    let submission = client
        .post(
            &format!("/api/challenges/{}/submissions", enc(&args.challenge_id)),
            body,
        )
        .await?;
    if !args.wait {
        return print_json(&submission);
    }

    let jobs = submission
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("submission response did not include jobs array"))?;
    let mut finished = Vec::new();
    for job in jobs {
        let job_id = job
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("job entry did not include job_id"))?;
        finished.push(watch_job(client, job_id, args.poll_interval_ms).await?);
    }

    print_json(&json!({
        "submission": submission,
        "jobs": finished,
    }))
}

async fn handle_jobs(args: JobsArgs, client: &ApiClient) -> Result<()> {
    match args.command {
        JobsCommand::Show { job_id } => print_json(&get_job(client, &job_id).await?),
        JobsCommand::Watch {
            job_id,
            poll_interval_ms,
        } => print_json(&watch_job(client, &job_id, poll_interval_ms).await?),
        JobsCommand::Queue { limit, cursor } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor));
            }
            print_json(&client.get("/api/jobs/non-done", &query).await?)
        }
    }
}

async fn handle_users(args: UsersArgs, client: &ApiClient) -> Result<()> {
    match args.command {
        UsersCommand::Jobs {
            user_id,
            challenge,
            limit,
            cursor,
        } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(challenge) = challenge {
                query.push(("challenge_id", challenge));
            }
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor));
            }
            let value = client
                .get(&format!("/api/users/{}/jobs", enc(&user_id)), &query)
                .await?;
            print_json(&value)
        }
    }
}

async fn handle_solutions(args: SolutionsArgs, client: &ApiClient) -> Result<()> {
    match args.command {
        SolutionsCommand::Show { solution_id } => print_json(
            &client
                .get(&format!("/api/solutions/{}", enc(&solution_id)), &[])
                .await?,
        ),
        SolutionsCommand::Jobs {
            solution_id,
            limit,
            cursor,
        } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor));
            }
            print_json(
                &client
                    .get(
                        &format!("/api/solutions/{}/jobs", enc(&solution_id)),
                        &query,
                    )
                    .await?,
            )
        }
    }
}

async fn get_job(client: &ApiClient, job_id: &str) -> Result<Value> {
    client.get(&format!("/api/jobs/{}", enc(job_id)), &[]).await
}

async fn watch_job(client: &ApiClient, job_id: &str, poll_interval_ms: u64) -> Result<Value> {
    let interval = Duration::from_millis(poll_interval_ms.max(100));
    loop {
        let job = get_job(client, job_id).await?;
        if job.get("status").and_then(Value::as_str) == Some("done") {
            return Ok(job);
        }
        sleep(interval).await;
    }
}

fn read_source(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .context("read source from stdin")?;
        Ok(source)
    } else {
        fs::read_to_string(path).with_context(|| format!("read source {}", path.display()))
    }
}

fn enc(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
