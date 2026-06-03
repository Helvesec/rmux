use std::collections::VecDeque;
use std::io;
use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use rmux_proto::RmuxError;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{lookup_host, TcpStream};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

use super::preset::{ProcessOutput, TunnelPreset};
use crate::web::origin::validate_public_base_url;
use crate::web::settings::WebShareSettings;
use crate::web::tunnel::TunnelInfo;

const LINE_CHANNEL_CAPACITY: usize = 64;
const ERROR_LINE_LIMIT: usize = 8;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct TunnelHandle {
    provider: String,
    stop_tx: Option<oneshot::Sender<()>>,
    _task: JoinHandle<()>,
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        debug!(provider = %self.provider, "stopping web-share tunnel provider");
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

pub(super) async fn start(
    preset: TunnelPreset,
    settings: &WebShareSettings,
) -> Result<TunnelInfo, RmuxError> {
    let regex = Regex::new(&preset.url_pattern).map_err(|error| {
        RmuxError::Server(format!(
            "web-share tunnel preset '{}' has an invalid url_pattern: {error}",
            preset.name
        ))
    })?;
    let ready_regex = preset
        .ready_pattern
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|error| {
            RmuxError::Server(format!(
                "web-share tunnel preset '{}' has an invalid ready_pattern: {error}",
                preset.name
            ))
        })?;
    let program = expand(&preset.program, settings)?;
    let args = preset
        .args
        .iter()
        .map(|arg| expand(arg, settings))
        .collect::<Result<Vec<_>, _>>()?;
    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| spawn_error(&preset, &program, error))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let (line_tx, line_rx) = mpsc::channel(LINE_CHANNEL_CAPACITY);
    tokio::spawn(read_stdout(stdout, line_tx.clone()));
    tokio::spawn(read_stderr(stderr, line_tx));

    let (stop_tx, stop_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        tokio::select! {
            _ = stop_rx => {
                terminate_child(&mut child);
                if timeout(STOP_GRACE_PERIOD, child.wait()).await.is_err() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
            status = child.wait() => {
                let _ = exit_tx.send(status);
            }
        }
    });
    let mut handle = Some(TunnelHandle {
        provider: preset.name.clone(),
        stop_tx: Some(stop_tx),
        _task: task,
    });
    let public_url =
        match wait_for_url(&preset, &regex, ready_regex.as_ref(), line_rx, exit_rx).await {
            Ok(url) => url,
            Err(error) => {
                drop(handle.take());
                return Err(error);
            }
        };
    if let Err(error) = wait_for_public_endpoint(&preset, &public_url).await {
        drop(handle.take());
        return Err(error);
    }
    info!(
        provider = %preset.name,
        public_url,
        "web_share_tunnel_ready"
    );
    Ok(TunnelInfo {
        handle: handle.expect("handle remains when tunnel starts"),
        provider: preset.name,
        public_url,
    })
}

async fn wait_for_public_endpoint(preset: &TunnelPreset, url: &str) -> Result<(), RmuxError> {
    let (host, port) = public_endpoint(url).map_err(|error| {
        RmuxError::Server(format!(
            "web-share tunnel provider '{}' printed an invalid public URL '{}': {error}",
            preset.name, url
        ))
    })?;
    let wait = async {
        loop {
            if endpoint_accepts_connections(&host, port).await.is_ok() {
                return Ok::<(), ()>(());
            }
            sleep(Duration::from_secs(1)).await;
        }
    };
    match timeout(Duration::from_secs(preset.ready_timeout_secs), wait)
        .await
        .map_err(|_| {
            RmuxError::Server(format!(
                "web-share tunnel provider '{}' printed '{}' but it did not become reachable within {}s",
                preset.name, url, preset.ready_timeout_secs
            ))
        })? {
        Ok(()) => Ok(()),
        Err(()) => unreachable!("public endpoint wait loop never returns an inner error"),
    }
}

async fn endpoint_accepts_connections(host: &str, port: u16) -> io::Result<()> {
    let mut addrs = lookup_host((host, port)).await?;
    let mut last_error = None;
    for addr in addrs.by_ref() {
        match timeout(Duration::from_secs(2), TcpStream::connect(addr)).await {
            Ok(Ok(_stream)) => return Ok(()),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection attempt timed out",
                ));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "public endpoint did not resolve to any address",
        )
    }))
}

fn public_endpoint(url: &str) -> io::Result<(String, u16)> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing scheme"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, raw_port))
            if !host.is_empty() && raw_port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let port = raw_port.parse::<u16>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port: {error}"),
                )
            })?;
            (host, port)
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid authority",
            ));
        }
        None => (authority, default_port(scheme)?),
    };
    if host.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "missing host"));
    }
    Ok((host.to_owned(), port))
}

fn default_port(scheme: &str) -> io::Result<u16> {
    match scheme {
        "http" => Ok(80),
        "https" => Ok(443),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported URL scheme",
        )),
    }
}

#[cfg(unix)]
fn terminate_child(child: &mut tokio::process::Child) {
    let Some(pid) = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
    else {
        return;
    };
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
}

#[cfg(not(unix))]
fn terminate_child(_child: &mut tokio::process::Child) {}

async fn wait_for_url(
    preset: &TunnelPreset,
    regex: &Regex,
    ready_regex: Option<&Regex>,
    mut lines: mpsc::Receiver<(ProcessOutput, String)>,
    mut exit_rx: oneshot::Receiver<io::Result<std::process::ExitStatus>>,
) -> Result<String, RmuxError> {
    let mut last_lines = VecDeque::new();
    let ready = async {
        let mut found_url = None;
        let mut provider_ready = ready_regex.is_none();
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some((source, line)) = line else {
                        return Err(tunnel_error(preset, "ended before printing a public URL", &last_lines));
                    };
                    remember_line(&mut last_lines, &line);
                    if !preset.url_source.accepts(source) {
                        continue;
                    }
                    if let Some(ready_regex) = ready_regex {
                        provider_ready |= ready_regex.is_match(&line);
                    }
                    if let Some(found) = regex.find(&line) {
                        found_url = Some(validate_public_base_url(found.as_str())?);
                    }
                    if provider_ready {
                        if let Some(url) = found_url.take() {
                            return Ok(url);
                        }
                    }
                }
                status = &mut exit_rx => {
                    let detail = match status {
                        Ok(Ok(status)) => format!("exited with {status} before printing a public URL"),
                        Ok(Err(error)) => format!("failed while waiting for tunnel process: {error}"),
                        Err(_) => "ended before printing a public URL".to_owned(),
                    };
                    return Err(tunnel_error(preset, &detail, &last_lines));
                }
            }
        }
    };
    timeout(Duration::from_secs(preset.ready_timeout_secs), ready)
        .await
        .map_err(|_| tunnel_error(preset, "timed out waiting for a public URL", &last_lines))?
}

async fn read_stdout(stdout: ChildStdout, tx: mpsc::Sender<(ProcessOutput, String)>) {
    read_lines(ProcessOutput::Stdout, stdout, tx).await;
}

async fn read_stderr(stderr: ChildStderr, tx: mpsc::Sender<(ProcessOutput, String)>) {
    read_lines(ProcessOutput::Stderr, stderr, tx).await;
}

async fn read_lines<R>(source: ProcessOutput, reader: R, tx: mpsc::Sender<(ProcessOutput, String)>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if tx.send((source, line)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                debug!("web-share tunnel output read failed: {error}");
                return;
            }
        }
    }
}

fn remember_line(lines: &mut VecDeque<String>, line: &str) {
    if lines.len() == ERROR_LINE_LIMIT {
        lines.pop_front();
    }
    lines.push_back(line.to_owned());
}

fn tunnel_error(preset: &TunnelPreset, detail: &str, lines: &VecDeque<String>) -> RmuxError {
    let mut message = format!("web-share tunnel provider '{}' {detail}", preset.name);
    if !lines.is_empty() {
        message.push_str(". Last output:\n");
        for line in lines {
            message.push_str("  ");
            message.push_str(line);
            message.push('\n');
        }
    }
    if let Some(hint) = preset.install_hint.as_deref() {
        message.push_str(". ");
        message.push_str(hint);
    }
    RmuxError::Server(message)
}

fn spawn_error(preset: &TunnelPreset, program: &str, error: io::Error) -> RmuxError {
    let mut message = format!(
        "failed to start web-share tunnel provider '{}' with '{}': {error}",
        preset.name, program
    );
    if error.kind() == io::ErrorKind::NotFound {
        if let Some(hint) = preset.install_hint.as_deref() {
            message.push_str(". ");
            message.push_str(hint);
        }
    }
    RmuxError::Server(message)
}

fn expand(value: &str, settings: &WebShareSettings) -> Result<String, RmuxError> {
    let expanded = value
        .replace("{host}", &settings.host)
        .replace("{port}", &settings.port.to_string());
    if expanded.contains('{') || expanded.contains('}') {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset contains an unknown placeholder in '{value}'"
        )));
    }
    Ok(expanded)
}

#[cfg(all(test, unix))]
mod tests {
    use super::start;
    use crate::web::settings::WebShareSettings;
    use crate::web::tunnel::preset::{TunnelPreset, UrlSource};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn runner_extracts_public_url_and_stops_on_drop() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind readiness probe listener");
        let port = listener.local_addr().expect("listener addr").port();
        let _listener_task =
            tokio::spawn(
                async move { while let Ok((_stream, _addr)) = listener.accept().await {} },
            );
        let url = format!("http://127.0.0.1:{port}");
        let preset = TunnelPreset {
            name: "test".to_owned(),
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), format!("printf '%s\\n' {url}; sleep 30")],
            url_pattern: regex::escape(&url),
            ready_pattern: None,
            url_source: UrlSource::Stdout,
            ready_timeout_secs: 5,
            install_hint: None,
        };
        let info = start(preset, &WebShareSettings::default())
            .await
            .expect("tunnel starts");
        assert_eq!(info.public_url, url);
        drop(info);
    }
}
