use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;

const DEFAULT_BASE_URL: &str = "https://cpu.mattstuchlik.com";
const USER_AGENT: &str = concat!("cpu-mode-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Parser)]
#[command(name = "cpu-mode")]
#[command(about = "Command-line client for cpu.mode")]
struct Cli {
    #[arg(long, global = true)]
    raw: bool,

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
    Profile {
        job_id: String,

        #[arg(short, long)]
        output: Option<PathBuf>,
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
    Publish {
        solution_id: String,
    },
    Unpublish {
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

    async fn get_text(&self, path: &str, query: &[(&str, String)]) -> Result<String> {
        self.request_text(Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, &[], Some(body)).await
    }

    async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::PATCH, path, &[], Some(body)).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let text = self.request_text(method, path, query, body).await?;
        parse_json_body(&text)
    }

    async fn request_text(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<String> {
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
        Ok(text)
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
    let token = store.token().map(str::to_string);
    let client = ApiClient::new(DEFAULT_BASE_URL.to_string(), token);
    let output = OutputMode { raw: cli.raw };

    match cli.command {
        Command::Auth(args) => handle_auth(args, &client, &mut store, output).await,
        Command::Challenges(args) => handle_challenges(args, &client, output).await,
        Command::Systems(args) => handle_systems(args, &client, output).await,
        Command::Leaderboard(args) => handle_leaderboard(args, &client, output).await,
        Command::Submit(args) => handle_submit(args, &client, output).await,
        Command::Jobs(args) => handle_jobs(args, &client, output).await,
        Command::Users(args) => handle_users(args, &client, output).await,
        Command::Solutions(args) => handle_solutions(args, &client, output).await,
    }
}

#[derive(Clone, Copy)]
struct OutputMode {
    raw: bool,
}

async fn handle_auth(
    args: AuthArgs,
    client: &ApiClient,
    store: &mut ConfigStore,
    output: OutputMode,
) -> Result<()> {
    match args.command {
        AuthCommand::Login(args) => auth_login(args, client, store, output).await,
        AuthCommand::Status => {
            let session = client.get("/auth/session", &[]).await?;
            output.print(&session, print_auth_status)
        }
        AuthCommand::Logout => {
            store.clear_token()?;
            output.print(
                &json!({"ok": true, "message": "local token removed"}),
                print_logout,
            )
        }
    }
}

async fn auth_login(
    args: AuthLoginArgs,
    client: &ApiClient,
    store: &mut ConfigStore,
    output: OutputMode,
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
                return output.print(
                    &json!({
                        "authenticated": true,
                        "stored": !args.no_store,
                        "user": poll.user,
                        "token": if args.no_store { Some(token) } else { None },
                    }),
                    print_auth_login,
                );
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

async fn handle_challenges(
    args: ChallengesArgs,
    client: &ApiClient,
    output: OutputMode,
) -> Result<()> {
    match args.command {
        ChallengesCommand::List => {
            let value = client.get("/api/challenges", &[]).await?;
            output.print(&value, print_challenge_list)
        }
        ChallengesCommand::Show { challenge_id } => {
            let value = client
                .get(&format!("/api/challenges/{}", enc(&challenge_id)), &[])
                .await?;
            output.print(&value, print_challenge_detail)
        }
    }
}

async fn handle_systems(args: SystemsArgs, client: &ApiClient, output: OutputMode) -> Result<()> {
    match args.command {
        SystemsCommand::List => {
            output.print(&client.get("/api/systems", &[]).await?, print_systems)
        }
    }
}

async fn handle_leaderboard(
    args: LeaderboardArgs,
    client: &ApiClient,
    output: OutputMode,
) -> Result<()> {
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
    output.print(&value, print_leaderboard)
}

async fn handle_submit(args: SubmitArgs, client: &ApiClient, output: OutputMode) -> Result<()> {
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
        return output.print(&submission, print_submission);
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

    output.print(
        &json!({
            "submission": submission,
            "jobs": finished,
        }),
        print_submission_with_jobs,
    )
}

async fn handle_jobs(args: JobsArgs, client: &ApiClient, output: OutputMode) -> Result<()> {
    match args.command {
        JobsCommand::Show { job_id } => output.print(&get_job(client, &job_id).await?, print_job),
        JobsCommand::Watch {
            job_id,
            poll_interval_ms,
        } => output.print(
            &watch_job(client, &job_id, poll_interval_ms).await?,
            print_job,
        ),
        JobsCommand::Queue { limit, cursor } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor));
            }
            output.print(
                &client.get("/api/jobs/non-done", &query).await?,
                print_jobs_page,
            )
        }
        JobsCommand::Profile {
            job_id,
            output: path,
        } => {
            let profile = client
                .get_text(&format!("/api/jobs/{}/profile", enc(&job_id)), &[])
                .await?;
            if output.raw {
                if path.is_some() {
                    bail!("--output cannot be used with --raw");
                }
                print!("{profile}");
                Ok(())
            } else {
                let path = path.unwrap_or_else(|| {
                    PathBuf::from(format!(
                        "{}.perf-annotate.txt",
                        safe_filename_component(&job_id)
                    ))
                });
                fs::write(&path, profile.as_bytes())
                    .with_context(|| format!("write profile {}", path.display()))?;
                println!(
                    "Downloaded profile to {} ({} bytes)",
                    path.display(),
                    profile.len()
                );
                Ok(())
            }
        }
    }
}

async fn handle_users(args: UsersArgs, client: &ApiClient, output: OutputMode) -> Result<()> {
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
            output.print(&value, print_jobs_page)
        }
    }
}

async fn handle_solutions(
    args: SolutionsArgs,
    client: &ApiClient,
    output: OutputMode,
) -> Result<()> {
    match args.command {
        SolutionsCommand::Show { solution_id } => output.print(
            &client
                .get(&format!("/api/solutions/{}", enc(&solution_id)), &[])
                .await?,
            print_solution,
        ),
        SolutionsCommand::Publish { solution_id } => output.print(
            &client
                .patch(
                    &format!("/api/solutions/{}", enc(&solution_id)),
                    json!({ "is_public": true }),
                )
                .await?,
            print_solution_visibility,
        ),
        SolutionsCommand::Unpublish { solution_id } => output.print(
            &client
                .patch(
                    &format!("/api/solutions/{}", enc(&solution_id)),
                    json!({ "is_public": false }),
                )
                .await?,
            print_solution_visibility,
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
            output.print(
                &client
                    .get(
                        &format!("/api/solutions/{}/jobs", enc(&solution_id)),
                        &query,
                    )
                    .await?,
                print_jobs_page,
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

impl OutputMode {
    fn print(self, value: &Value, human: fn(&Value) -> Result<()>) -> Result<()> {
        if self.raw {
            print_json(value)
        } else {
            human(value)
        }
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_auth_status(value: &Value) -> Result<()> {
    if value.get("authenticated").and_then(Value::as_bool) == Some(true) {
        println!("Authenticated");
        if let Some(user) = value.get("user") {
            print_user_line(user);
        }
    } else {
        println!("Not authenticated");
    }
    Ok(())
}

fn print_auth_login(value: &Value) -> Result<()> {
    println!("Authenticated");
    if let Some(user) = value.get("user") {
        print_user_line(user);
    }
    if value.get("stored").and_then(Value::as_bool) == Some(true) {
        println!("Token stored locally");
    } else if let Some(token) = value.get("token").and_then(Value::as_str) {
        println!("Token: {token}");
    }
    Ok(())
}

fn print_logout(_: &Value) -> Result<()> {
    println!("Local token removed");
    Ok(())
}

fn print_user_line(user: &Value) {
    let name = first_str(user, &["display_name", "name", "login"]).unwrap_or("-");
    let id = first_str(user, &["id", "user_id"]).unwrap_or("-");
    println!("User: {name} ({id})");
}

fn print_challenge_list(value: &Value) -> Result<()> {
    let challenges = value
        .get("challenges")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("challenge list response did not include challenges array"))?;

    let id_width = challenges
        .iter()
        .filter_map(|challenge| challenge.get("id").and_then(Value::as_str))
        .map(str::len)
        .chain(std::iter::once("ID".len()))
        .max()
        .unwrap_or("ID".len());
    let title_width = challenges
        .iter()
        .filter_map(|challenge| challenge.get("title").and_then(Value::as_str))
        .map(str::len)
        .chain(std::iter::once("TITLE".len()))
        .max()
        .unwrap_or("TITLE".len());

    println!("{:<id_width$}  {:<title_width$}  LANGUAGES", "ID", "TITLE");
    println!("{:-<id_width$}  {:-<title_width$}  ---------", "", "");

    for challenge in challenges {
        let id = challenge.get("id").and_then(Value::as_str).unwrap_or("-");
        let title = challenge
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let languages = challenge
            .get("languages")
            .and_then(Value::as_array)
            .map(|languages| {
                languages
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|languages| !languages.is_empty())
            .unwrap_or_else(|| "-".to_string());
        println!("{id:<id_width$}  {title:<title_width$}  {languages}");
    }

    Ok(())
}

fn print_challenge_detail(value: &Value) -> Result<()> {
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    let title = value.get("title").and_then(Value::as_str).unwrap_or("-");
    println!("{title} ({id})");

    if let Some(description) = value.get("description").and_then(Value::as_array) {
        println!();
        for line in description.iter().filter_map(Value::as_str) {
            println!("{line}");
        }
    }

    println!();
    println!(
        "Languages: {}",
        string_list(value.get("languages")).unwrap_or_else(|| "-".to_string())
    );

    if let Some(limits) = value.get("limits").and_then(Value::as_object) {
        if let Some(source_bytes) = limits.get("source_bytes").and_then(Value::as_u64) {
            println!("Source limit: {}", format_bytes(source_bytes));
        }
        if let Some(options_bytes) = limits.get("compiler_options_bytes").and_then(Value::as_u64) {
            println!("Compiler options limit: {}", format_bytes(options_bytes));
        }
    }

    if let Some(options) = value.get("compiler_options").and_then(Value::as_object) {
        println!();
        println!("Compiler defaults:");
        if let Some(rust) = options.get("rust_default").and_then(Value::as_str) {
            println!("  rust: {rust}");
        }
        if let Some(cpp) = options.get("cpp_default").and_then(Value::as_str) {
            println!("  cpp:  {cpp}");
        }
    }

    if let Some(urls) = value.get("urls").and_then(Value::as_object) {
        println!();
        println!("API paths:");
        for key in ["leaderboard", "record_history", "submissions"] {
            if let Some(url) = urls.get(key).and_then(Value::as_str) {
                println!("  {key}: {url}");
            }
        }
    }

    Ok(())
}

fn print_systems(value: &Value) -> Result<()> {
    let systems = value
        .as_array()
        .ok_or_else(|| anyhow!("systems response was not an array"))?;
    let rows = systems
        .iter()
        .map(|system| {
            vec![
                field_string(system, "id"),
                field_string(system, "label"),
                field_string(system, "uarch"),
                field_u64(system, "cpu")
                    .map(|cpu| cpu.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["ID", "LABEL", "UARCH", "CPU"], &rows);
    Ok(())
}

fn print_leaderboard(value: &Value) -> Result<()> {
    let challenge = field_string(value, "challenge_id");
    let system = field_string(value, "system_id");
    println!("{challenge} / {system}");

    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("leaderboard response did not include entries array"))?;
    if entries.is_empty() {
        println!("No leaderboard entries.");
        return Ok(());
    }

    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                field_u64(entry, "rank")
                    .map(|rank| rank.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                field_string(entry, "user_display_name"),
                if entry
                    .get("solution_is_public")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "public".to_string()
                } else {
                    "-".to_string()
                },
                field_string(entry, "language"),
                ns_field(entry, "time_ns"),
                field_u64(entry, "cycles")
                    .map(format_count)
                    .unwrap_or_else(|| "-".to_string()),
                field_string(entry, "job_id"),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &["RANK", "USER", "PUBLIC", "LANG", "TIME", "CYCLES", "JOB"],
        &rows,
    );
    Ok(())
}

fn print_submission(value: &Value) -> Result<()> {
    println!("Solution: {}", field_string(value, "solution_id"));
    println!("User: {}", field_string(value, "user_id"));
    print_submission_jobs(value);
    Ok(())
}

fn print_submission_with_jobs(value: &Value) -> Result<()> {
    let submission = value
        .get("submission")
        .ok_or_else(|| anyhow!("wait response did not include submission"))?;
    print_submission(submission)?;

    println!();
    println!("Finished jobs:");
    let jobs = value
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("wait response did not include jobs array"))?;
    print_job_rows(jobs);
    Ok(())
}

fn print_submission_jobs(value: &Value) {
    let Some(jobs) = value.get("jobs").and_then(Value::as_array) else {
        return;
    };
    if jobs.is_empty() {
        println!("Jobs: none");
        return;
    }
    let rows = jobs
        .iter()
        .map(|job| vec![field_string(job, "system_id"), field_string(job, "job_id")])
        .collect::<Vec<_>>();
    println!("Jobs:");
    print_table(&["SYSTEM", "JOB"], &rows);
}

fn print_job(value: &Value) -> Result<()> {
    println!("Job: {}", field_string(value, "id"));
    println!("Challenge: {}", field_string(value, "challenge_id"));
    println!("Solution: {}", field_string(value, "solution_id"));
    println!("System: {}", field_string(value, "system_id"));
    println!("Language: {}", field_string(value, "language"));
    println!("Status: {}", field_string(value, "status"));
    if let Some(time) = field_u64(value, "result_time_ns") {
        println!("Time: {}", format_ns(time));
    }
    if let Some(time) = field_u64(value, "result_time_max_ns") {
        println!("Max time: {}", format_ns(time));
    }
    if let Some(cycles) = field_u64(value, "result_cycles") {
        println!("Cycles: {}", format_count(cycles));
    }
    if let Some(error) = value.get("result_error").and_then(Value::as_str) {
        println!("Error: {error}");
    }
    if let Some(counters) = value.get("result_counters").and_then(Value::as_object) {
        if !counters.is_empty() {
            println!("Counters:");
            for (name, count) in counters {
                if let Some(count) = count.as_u64() {
                    println!("  {name}: {}", format_count(count));
                }
            }
        }
    }
    Ok(())
}

fn print_jobs_page(value: &Value) -> Result<()> {
    let jobs = value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| anyhow!("jobs response did not include items array"))?;
    if jobs.is_empty() {
        println!("No jobs.");
    } else {
        print_job_rows(jobs);
    }
    if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
        println!();
        println!("Next cursor: {cursor}");
    }
    Ok(())
}

fn print_job_rows(jobs: &[Value]) {
    let rows = jobs
        .iter()
        .map(|job| {
            vec![
                field_string(job, "id"),
                field_string(job, "challenge_id"),
                field_string(job, "system_id"),
                field_string(job, "language"),
                field_string(job, "status"),
                ns_field(job, "result_time_ns"),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &["JOB", "CHALLENGE", "SYSTEM", "LANG", "STATUS", "TIME"],
        &rows,
    );
}

fn print_solution(value: &Value) -> Result<()> {
    println!("Solution: {}", field_string(value, "id"));
    println!("Challenge: {}", field_string(value, "challenge_id"));
    println!(
        "User: {} ({})",
        field_string(value, "user_display_name"),
        field_string(value, "user_id")
    );
    println!("Language: {}", field_string(value, "language"));
    println!(
        "Visibility: {}",
        if value
            .get("is_public")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "public"
        } else {
            "private"
        }
    );
    if let Some(jobs_url) = value.get("jobs_url").and_then(Value::as_str) {
        println!("Jobs: {jobs_url}");
    }

    let source_visible = value
        .get("source_visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if source_visible {
        if let Some(options) = value.get("compiler_options").and_then(Value::as_str) {
            println!();
            println!("Compiler options:");
            println!("{options}");
        }
        if let Some(source) = value.get("source").and_then(Value::as_str) {
            println!();
            println!("Source:");
            println!("{source}");
        }
    } else {
        println!("Source: hidden");
    }
    Ok(())
}

fn print_solution_visibility(value: &Value) -> Result<()> {
    println!("Solution: {}", field_string(value, "solution_id"));
    println!(
        "Visibility: {}",
        if value
            .get("is_public")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "public"
        } else {
            "private"
        }
    );
    Ok(())
}

fn string_list(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|items| !items.is_empty())
}

fn format_bytes(bytes: u64) -> String {
    if bytes % 1024 == 0 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

fn first_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn field_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string()
}

fn field_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn ns_field(value: &Value, key: &str) -> String {
    field_u64(value, key)
        .map(format_ns)
        .unwrap_or_else(|| "-".to_string())
}

fn format_ns(ns: u64) -> String {
    format!("{:.3} ms", ns as f64 / 1_000_000.0)
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        let remaining = digits.len() - index;
        if index > 0 && remaining % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn safe_filename_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.len());
            }
        }
    }

    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{header:<width$}", width = widths[index]);
    }
    println!();

    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{:-<width$}", "");
    }
    println!();

    for row in rows {
        for (index, width) in widths.iter().enumerate() {
            if index > 0 {
                print!("  ");
            }
            let cell = row.get(index).map(String::as_str).unwrap_or("-");
            print!("{cell:<width$}");
        }
        println!();
    }
}
