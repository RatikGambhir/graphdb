//! Process management: `start`, `stop`, `restart`, `status`, `logs` and
//! `service` (launchd on macOS, systemd on Linux).

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::Value as J;

use crate::server::{ServeOpts, ServerState};
use crate::{brand, paths};

const LAUNCHD_LABEL: &str = "com.ratikdb.ratik";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

/// GET or POST a JSON endpoint; `Ok(None)` when nothing is listening.
pub fn call(method: &str, url: &str, token: Option<&str>) -> anyhow::Result<Option<J>> {
    let agent = agent();
    let auth = token.map(|t| format!("Bearer {t}"));
    let sent = if method == "GET" {
        let mut r = agent.get(url);
        if let Some(a) = &auth {
            r = r.header("Authorization", a);
        }
        r.call()
    } else {
        let mut r = agent.post(url);
        if let Some(a) = &auth {
            r = r.header("Authorization", a);
        }
        r.send_empty()
    };
    let Ok(mut resp) = sent else { return Ok(None) };
    let status = resp.status().as_u16();
    let body: J = resp.body_mut().read_json().unwrap_or(J::Null);
    if status == 401 {
        bail!("the server needs a token — pass --token or set RATIK_TOKEN");
    }
    if !(200..300).contains(&status) {
        bail!(
            "{url} answered HTTP {status}: {}",
            body["error"]["message"].as_str().unwrap_or_default()
        );
    }
    Ok(Some(body))
}

pub fn read_state(data: &Path) -> Option<ServerState> {
    serde_json::from_slice(&fs::read(paths::state_file(data)).ok()?).ok()
}

pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn signal(pid: u32, sig: &str) {
    let _ = Command::new("kill")
        .args([sig, &pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

fn local_url(host: &str, port: u16) -> String {
    let host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1"
    } else {
        host
    };
    format!("http://{host}:{port}")
}

fn tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let lines: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// Arguments that make a child `ratik serve` behave like `opts`.
fn serve_args(opts: &ServeOpts) -> Vec<String> {
    let mut args = vec![
        "serve".into(),
        "--data".into(),
        opts.data.display().to_string(),
        "--host".into(),
        opts.host.clone(),
        "--port".into(),
        opts.port.to_string(),
        "--sync".into(),
        match opts.sync {
            ratikdb::SyncMode::Never => "never".into(),
            _ => "always".into(),
        },
        "--cache-mb".into(),
        opts.cache_mb.to_string(),
        "--quiet".into(),
    ];
    if opts.memory {
        args.push("--memory".into());
    }
    for f in &opts.apply {
        args.push("--apply".into());
        args.push(
            fs::canonicalize(f)
                .unwrap_or_else(|_| f.clone())
                .display()
                .to_string(),
        );
    }
    for o in &opts.cors {
        args.push("--cors".into());
        args.push(o.clone());
    }
    args
}

pub fn start(opts: &ServeOpts) -> anyhow::Result<()> {
    fs::create_dir_all(&opts.data)
        .with_context(|| format!("cannot create {}", opts.data.display()))?;
    if let Some(st) = read_state(&opts.data)
        && pid_alive(st.pid)
    {
        println!(
            "{}",
            brand::ok(&format!(
                "RatikDB is already running at {} (pid {})",
                st.url, st.pid
            ))
        );
        return Ok(());
    }
    let log = paths::log_file(&opts.data);
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let out = OpenOptions::new().create(true).append(true).open(&log)?;
    let log_start = out.metadata().map_or(0, |m| m.len());
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(serve_args(opts))
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    if let Some(t) = &opts.token {
        // Pass secrets through the environment, not argv (visible in `ps`).
        cmd.env("RATIK_TOKEN", t);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: closing the terminal does not stop the server.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().context("could not launch the server")?;
    let url = local_url(&opts.host, opts.port);
    let health = format!("{url}/v1/health");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            let mut f = fs::File::open(&log)?;
            f.seek(SeekFrom::Start(log_start))?;
            let mut text = String::new();
            f.read_to_string(&mut text)?;
            bail!("the server exited ({status}):\n{}", text.trim());
        }
        if matches!(call("GET", &health, None), Ok(Some(_))) {
            println!("{} {}", brand::mark(), brand::dim("is up"));
            println!("{}", brand::kv("api", &brand::rgb(&url, brand::CYAN)));
            println!(
                "{}",
                brand::kv("console", &brand::rgb(&format!("{url}/"), brand::CYAN))
            );
            println!("{}", brand::kv("pid", &child.id().to_string()));
            let storage = if opts.memory {
                "in-memory".to_owned()
            } else {
                opts.data.display().to_string()
            };
            println!("{}", brand::kv("data", &storage));
            println!("{}", brand::kv("logs", &log.display().to_string()));
            println!(
                "\n  {}",
                brand::dim("ratik shell · ratik status · ratik stop")
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "the server did not become healthy within 30s — see {}",
        log.display()
    )
}

/// Resolves which server a control command talks to.
pub fn target(data: &Path, url: Option<&str>) -> (String, Option<ServerState>) {
    let state = read_state(data);
    let url = url
        .map(str::to_owned)
        .or_else(|| state.as_ref().map(|s| s.url.clone()))
        .unwrap_or_else(|| ratik::DEFAULT_URL.to_owned());
    (url, state)
}

pub fn stop(data: &Path, url: Option<&str>, token: Option<&str>) -> anyhow::Result<bool> {
    let (url, state) = target(data, url);
    let asked = call("POST", &format!("{url}/v1/admin/shutdown"), token)?.is_some();
    let pid = state.as_ref().map(|s| s.pid).filter(|p| pid_alive(*p));
    if !asked && pid.is_none() {
        println!(
            "{}",
            brand::warn(&format!("RatikDB is not running at {url}"))
        );
        let _ = fs::remove_file(paths::state_file(data));
        return Ok(false);
    }
    if let Some(pid) = pid {
        if !asked {
            signal(pid, "-TERM");
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while pid_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if pid_alive(pid) {
            println!("{}", brand::warn("graceful shutdown timed out; forcing"));
            signal(pid, "-KILL");
        }
    } else {
        std::thread::sleep(Duration::from_millis(300));
    }
    println!("{}", brand::ok("RatikDB stopped cleanly"));
    Ok(true)
}

pub fn status(data: &Path, url: Option<&str>, token: Option<&str>) -> anyhow::Result<bool> {
    let (url, state) = target(data, url);
    let Some(stats) = call("GET", &format!("{url}/v1/stats"), token)? else {
        println!(
            "{} {}",
            brand::mark(),
            brand::rgb("● stopped", brand::EMBER)
        );
        println!("{}", brand::kv("checked", &url));
        if let Some(s) = state
            && !pid_alive(s.pid)
        {
            println!(
                "{}",
                brand::kv(
                    "note",
                    "found a stale server.json (the last run did not exit cleanly)"
                )
            );
        }
        println!("\n  {}", brand::dim("start it with: ratik start"));
        return Ok(false);
    };
    let uptime = stats["uptime_secs"].as_u64().unwrap_or(0);
    let human = match uptime {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    };
    println!("{} {}", brand::mark(), brand::rgb("● running", brand::VOLT));
    println!("{}", brand::kv("api", &brand::rgb(&url, brand::CYAN)));
    println!("{}", brand::kv("pid", &stats["pid"].to_string()));
    println!("{}", brand::kv("uptime", &human));
    println!(
        "{}",
        brand::kv("version", stats["version"].as_str().unwrap_or("?"))
    );
    println!(
        "{}",
        brand::kv("data", stats["data"].as_str().unwrap_or("in-memory"))
    );
    println!(
        "{}",
        brand::kv(
            "graph",
            &format!(
                "{} nodes · {} edges · {} node types · {} edge types · {} stored queries",
                stats["nodes"],
                stats["edges"],
                stats["node_types"],
                stats["edge_types"],
                stats["stored_queries"]
            )
        )
    );
    println!(
        "{}",
        brand::kv(
            "disk",
            &format!(
                "{:.1} MiB in {}",
                stats["disk_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0),
                ratikdb::DATA_FILE
            )
        )
    );
    Ok(true)
}

pub fn logs(data: &Path, lines: usize, follow: bool) -> anyhow::Result<()> {
    let path = paths::log_file(data);
    if !path.exists() {
        bail!(
            "no log file yet at {} (logs are written by `ratik start`)",
            path.display()
        );
    }
    for line in tail(&path, lines) {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }
    let mut f = fs::File::open(&path)?;
    let mut pos = f.seek(SeekFrom::End(0))?;
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let len = fs::metadata(&path)?.len();
        if len < pos {
            pos = 0; // rotated / truncated
        }
        if len > pos {
            f.seek(SeekFrom::Start(pos))?;
            let mut chunk = String::new();
            f.read_to_string(&mut chunk)?;
            print!("{chunk}");
            pos = len;
        }
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("launchctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "501".into(), |s| s.trim().to_owned())
}

/// The launchd agent (macOS) or systemd unit (Linux) text.
pub fn service_definition(exe: &Path, opts: &ServeOpts) -> String {
    render_service(exe, opts, cfg!(target_os = "macos"))
}

fn render_service(exe: &Path, opts: &ServeOpts, launchd: bool) -> String {
    let args = serve_args(opts);
    let log = paths::log_file(&opts.data);
    if launchd {
        let program: String = std::iter::once(exe.display().to_string())
            .chain(args)
            .map(|a| format!("\n      <string>{}</string>", xml(&a)))
            .collect();
        let env = opts.token.as_ref().map_or_else(String::new, |t| {
            format!(
                "\n    <key>EnvironmentVariables</key>\n    <dict>\n      <key>RATIK_TOKEN</key>\n      <string>{}</string>\n    </dict>",
                xml(t)
            )
        });
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>{program}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
      <key>SuccessfulExit</key>
      <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>{env}
  </dict>
</plist>
"#,
            log = xml(&log.display().to_string()),
        )
    } else {
        let quoted: Vec<String> = std::iter::once(exe.display().to_string())
            .chain(args)
            .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
            .collect();
        let env = opts
            .token
            .as_ref()
            .map_or_else(String::new, |t| format!("Environment=RATIK_TOKEN={t}\n"));
        format!(
            "[Unit]\nDescription=RatikDB graph database\nAfter=network.target\n\n[Service]\nExecStart={}\n{env}Restart=on-failure\nStandardOutput=append:{log}\nStandardError=append:{log}\n\n[Install]\nWantedBy=default.target\n",
            quoted.join(" "),
            log = log.display()
        )
    }
}

pub fn service_install(opts: &ServeOpts) -> anyhow::Result<()> {
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let file = paths::service_file();
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    if let Some(dir) = paths::log_file(&opts.data).parent() {
        fs::create_dir_all(dir)?;
    }
    fs::create_dir_all(&opts.data)?;
    fs::write(&file, service_definition(&exe, opts))?;
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", uid());
        let _ = launchctl(&["bootout", &format!("{domain}/{LAUNCHD_LABEL}")]);
        if !launchctl(&["bootstrap", &domain, &file.display().to_string()]) {
            bail!(
                "launchctl could not load {} — try: launchctl bootstrap {domain} {}",
                file.display(),
                file.display()
            );
        }
    } else {
        let ok = |args: &[&str]| {
            Command::new("systemctl")
                .arg("--user")
                .args(args)
                .status()
                .is_ok_and(|s| s.success())
        };
        if !(ok(&["daemon-reload"]) && ok(&["enable", "--now", "ratikdb.service"])) {
            bail!("systemctl could not enable {}", file.display());
        }
    }
    println!(
        "{}",
        brand::ok("installed RatikDB as a background service (starts at login, restarts on crash)")
    );
    println!("{}", brand::kv("service", &file.display().to_string()));
    println!("{}", brand::kv("api", &local_url(&opts.host, opts.port)));
    println!(
        "{}",
        brand::kv("logs", &paths::log_file(&opts.data).display().to_string())
    );
    Ok(())
}

pub fn service_uninstall() -> anyhow::Result<()> {
    let file: PathBuf = paths::service_file();
    if cfg!(target_os = "macos") {
        let _ = launchctl(&["bootout", &format!("gui/{}/{LAUNCHD_LABEL}", uid())]);
    } else {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "ratikdb.service"])
            .status();
    }
    if file.exists() {
        fs::remove_file(&file)?;
        println!("{}", brand::ok(&format!("removed {}", file.display())));
    } else {
        println!("{}", brand::warn("no RatikDB service was installed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ServeOpts {
        ServeOpts {
            data: PathBuf::from("/Users/ada/Library/Application Support/RatikDB"),
            memory: false,
            host: "127.0.0.1".into(),
            port: 7285,
            token: Some("t<&>".into()),
            sync: ratikdb::SyncMode::Always,
            apply: Vec::new(),
            cors: Vec::new(),
            cache_mb: 256,
            quiet: false,
        }
    }

    #[test]
    fn launchd_plist_is_escaped_and_restarts_only_on_crash() {
        let plist = render_service(Path::new("/opt/homebrew/bin/ratik"), &opts(), true);
        if let Some(out) = std::env::var_os("RATIK_DUMP_PLIST") {
            std::fs::write(out, &plist).unwrap();
        }
        assert!(plist.contains("<string>com.ratikdb.ratik</string>"));
        assert!(plist.contains("<string>/opt/homebrew/bin/ratik</string>"));
        assert!(
            plist.contains("<string>t&lt;&amp;&gt;</string>"),
            "token is XML-escaped"
        );
        assert!(
            plist.contains("<key>SuccessfulExit</key>\n      <false/>"),
            "`ratik stop` is not undone by launchd"
        );
        assert!(!plist.contains("t<&>"));
    }

    #[test]
    fn systemd_unit_keeps_token_out_of_argv() {
        let unit = render_service(Path::new("/usr/local/bin/ratik"), &opts(), false);
        assert!(unit.contains("Environment=RATIK_TOKEN="));
        assert!(!unit.contains("--token"));
    }
}
